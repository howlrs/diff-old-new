"""KPI K2: active session 開始時の Oracle ワープギャップ分布 (Issue #22).

closure → active 切替時, oracle が外部 (CME EMM6) に瞬時にワープするため
HL の last internal price と active 開始 N 分間の oracle 平均にギャップが生じる.
これが戦略 H1 の「monday open gap」素材.

入力: data/curated/features/*.parquet
出力: docs/kpi/K2.{md,json}
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any

import polars as pl

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from _common import safe_read_parquet_glob  # noqa: E402

from src.config import load_config  # noqa: E402
from src.l2_features.regime import Regime  # noqa: E402

TARGET_SYMBOL = "xyz:SP500"
PRE_WINDOW_MIN = 5
POST_WINDOW_MIN = 5


def _summarize(values: list[float]) -> dict[str, Any]:
    if not values:
        return {"n": 0}
    n = len(values)
    mean = sum(values) / n
    var = sum((v - mean) ** 2 for v in values) / n
    std = math.sqrt(var) if var > 0 else 0.0
    sv = sorted(values)
    return {
        "n": n,
        "mean": mean,
        "std": std,
        "median": sv[int(0.5 * (n - 1))],
        "p05": sv[int(0.05 * (n - 1))],
        "p95": sv[int(0.95 * (n - 1))],
        "min": sv[0],
        "max": sv[-1],
    }


def compute_k2(symbol: str = TARGET_SYMBOL) -> dict[str, Any]:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    glob = str(cfg.storage.curated_data_root / "features" / "**" / "*.parquet")
    df = safe_read_parquet_glob(glob)
    if df.is_empty():
        return {"symbol": symbol, "warning": "no curated features yet"}
    df = df.filter(pl.col("symbol") == symbol)
    if df.is_empty():
        return {"symbol": symbol, "warning": "no data for symbol"}

    df = df.sort("exchange_ts")

    # closure → active 切替検出
    df = df.with_columns(pl.col("regime").shift(1).alias("prev_regime"))
    closures = {
        Regime.CLOSURE_WEEKEND.value,
        Regime.CLOSURE_DAILY.value,
        Regime.CLOSURE_HOLIDAY.value,
    }

    transitions = df.filter(
        (pl.col("regime") == Regime.ACTIVE.value) & (pl.col("prev_regime").is_in(list(closures)))
    )

    by_prev_regime: dict[str, list[float]] = {}
    samples: list[dict[str, Any]] = []
    for row in transitions.iter_rows(named=True):
        ts = row["exchange_ts"]
        prev_regime = row["prev_regime"]
        # pre = closure 終了直前 N 分の HL 内部 mid
        pre = df.filter(
            (pl.col("exchange_ts") < ts)
            & (pl.col("exchange_ts") >= ts - pl.duration(minutes=PRE_WINDOW_MIN))
        )["mid"].drop_nulls()
        post = df.filter(
            (pl.col("exchange_ts") >= ts)
            & (pl.col("exchange_ts") < ts + pl.duration(minutes=POST_WINDOW_MIN))
        )["oracle_px"].drop_nulls()

        if len(pre) == 0 or len(post) == 0:
            continue
        pre_mean = float(pre.mean() or 0)
        post_mean = float(post.mean() or 0)
        if pre_mean == 0:
            continue
        gap_bps = (post_mean - pre_mean) / pre_mean * 10000.0
        by_prev_regime.setdefault(prev_regime, []).append(gap_bps)
        samples.append(
            {
                "transition_ts": str(ts),
                "prev_regime": prev_regime,
                "pre_mean": pre_mean,
                "post_mean": post_mean,
                "gap_bps": gap_bps,
            }
        )

    return {
        "symbol": symbol,
        "n_transitions": int(transitions.height),
        "n_with_gap": sum(len(v) for v in by_prev_regime.values()),
        "by_prev_regime": {
            k: {"distribution": _summarize(v), "n": len(v)} for k, v in by_prev_regime.items()
        },
        "samples_first10": samples[:10],
    }


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "K2.md"
    out_json = out_dir / "K2.json"
    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append(f"# KPI K2: active session 開始時 Oracle ワープギャップ — `{result['symbol']}`")
    lines.append("")
    lines.append(
        "v3 design §3 KPI 2. closure → active 切替時の HL内部mid vs 直後 oracle 平均の bps ギャップ."
    )
    lines.append("")
    lines.append(
        f"- transitions: {result.get('n_transitions', 0)}  /  with gap: {result.get('n_with_gap', 0)}"
    )
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")

    by = result.get("by_prev_regime", {})
    if by:
        lines.append("## prev_regime 別 ギャップ分布 (bps)")
        lines.append("")
        lines.append("| prev_regime | n | mean | std | p05 | median | p95 |")
        lines.append("|---|---|---|---|---|---|---|")
        for regime, stat in by.items():
            d = stat["distribution"]
            if d["n"] == 0:
                continue
            lines.append(
                f"| {regime} | {d['n']} | {d['mean']:+.2f} | {d['std']:.2f} | "
                f"{d['p05']:+.2f} | {d['median']:+.2f} | {d['p95']:+.2f} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append("- **ギャップ symbol**: 正なら HL内部 < CME (closure 中過小評価), 負なら過大評価")
    lines.append("- **ギャップが大きい (>10 bps)** prev_regime は戦略 H1 の優先ターゲット")
    lines.append(
        "- **mean が ~0 で std 大** はファットテール → 単純な mean reversion でなく分布 tail を狙う必要"
    )
    lines.append("")
    lines.append("## 次のステップ")
    lines.append("")
    lines.append("1. 1 週間 collect 後に R3 (CMEメンテ) → R1 transition で N >= 5 を取得")
    lines.append("2. 1ヶ月で R2 (週末) → R1 transition で N=4 → 累積で十分な統計")
    lines.append("3. ギャップ分布の自相関 (consecutive transitions の独立性) を確認")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute_k2(TARGET_SYMBOL)
    md = write_report(result)
    print(f"K2 report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
