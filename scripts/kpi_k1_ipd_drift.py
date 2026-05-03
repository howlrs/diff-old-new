"""KPI K1: closure 中 SP500 IPD 累積ドリフト分布 (週末/メンテ/祝日 別).

v3 §3 K1.

入力: data/curated/features/*.parquet
出力: docs/kpi/K1.md (text レポート) + 統計 JSON

Phase 1 実データが蓄積されるまでは, 既存の curated に対して計算する.
データが薄い間は分布の形状は不安定だが, パイプラインが動くことを示す.
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any

import duckdb
import polars as pl

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.config import load_config  # noqa: E402
from src.l2_features.regime import Regime  # noqa: E402

CLOSURE_REGIMES = [
    Regime.CLOSURE_WEEKEND.value,
    Regime.CLOSURE_DAILY.value,
    Regime.CLOSURE_HOLIDAY.value,
]

TARGET_SYMBOL = "xyz:SP500"


def _segment_cumulative_ipd(df: pl.DataFrame) -> pl.DataFrame:
    """closure regime 内で連続セグメントを切り, 累積 IPD を計算.

    regime が変わるたびに累積をリセットする.
    """
    df = df.sort("exchange_ts")
    seg_change = (pl.col("regime") != pl.col("regime").shift(1)).fill_null(True)
    df = df.with_columns(seg_change.cum_sum().alias("segment_id"))
    df = df.with_columns(pl.col("ipd").cum_sum().over(["symbol", "segment_id"]).alias("cum_ipd"))
    return df


def _summarize_distribution(values: list[float]) -> dict[str, Any]:
    if not values:
        return {"n": 0}
    n = len(values)
    mean = sum(values) / n
    var = sum((v - mean) ** 2 for v in values) / n
    std = math.sqrt(var) if var > 0 else 0.0
    sorted_vals = sorted(values)

    def pct(p: float) -> float:
        idx = int(p * (n - 1))
        return sorted_vals[idx]

    return {
        "n": n,
        "mean": mean,
        "std": std,
        "min": sorted_vals[0],
        "p05": pct(0.05),
        "p25": pct(0.25),
        "median": pct(0.5),
        "p75": pct(0.75),
        "p95": pct(0.95),
        "max": sorted_vals[-1],
        "skew_proxy": (mean - pct(0.5)) / std if std > 0 else 0.0,
    }


def compute_k1(symbol: str = TARGET_SYMBOL) -> dict[str, Any]:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    glob = str(cfg.storage.curated_data_root / "features" / "**" / "*.parquet")
    con = duckdb.connect(":memory:")
    arrow_tbl = con.execute(
        f"SELECT * FROM read_parquet('{glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' AND regime IN "
        f"({','.join(repr(r) for r in CLOSURE_REGIMES)})"
    ).arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()

    if df.is_empty():
        return {"symbol": symbol, "n_total": 0, "warning": "no closure data"}

    df = _segment_cumulative_ipd(df)

    # regime 別に segment 終端 cumulative IPD を集計
    by_regime: dict[str, dict[str, Any]] = {}
    for regime in CLOSURE_REGIMES:
        sub = df.filter(pl.col("regime") == regime)
        if sub.is_empty():
            by_regime[regime] = {"n": 0}
            continue
        # セグメント末尾の累積 IPD を集める (1セグメント = 1サンプル)
        segment_terminals = (
            sub.group_by("segment_id")
            .agg(pl.col("cum_ipd").last().alias("terminal_cum_ipd"))
            .drop_nulls()
        )
        terminals = [float(v) for v in segment_terminals["terminal_cum_ipd"].to_list()]
        by_regime[regime] = {
            "n_bars": len(sub),
            "n_segments": len(terminals),
            "terminal_cum_ipd_distribution": _summarize_distribution(terminals),
            # bar 単位の IPD 分布 (LLN 適用可否確認用)
            "bar_ipd_distribution": _summarize_distribution(
                [float(v) for v in sub["ipd"].drop_nulls().to_list()]
            ),
        }

    return {
        "symbol": symbol,
        "n_total_bars": int(df.height),
        "by_regime": by_regime,
        "computed_at": str(pl.lit(None)),  # placeholder
    }


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "K1.md"
    out_json = out_dir / "K1.json"

    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append(f"# KPI K1: closure 中 {result['symbol']} IPD 累積ドリフト分布")
    lines.append("")
    lines.append("v3 design §3 KPI 1. closure regime ごとに累積 IPD の分布を集計.")
    lines.append("")
    lines.append(f"- 対象 symbol: `{result['symbol']}`")
    lines.append(f"- 全 bars: {result.get('n_total_bars', 0)}")
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")

    if "by_regime" in result:
        lines.append("## regime 別 サマリ")
        lines.append("")
        lines.append(
            "| regime | n_segments | n_bars | terminal_cum_ipd mean | std | p05 | median | p95 |"
        )
        lines.append("|---|---|---|---|---|---|---|---|")
        for regime, stat in result["by_regime"].items():
            if stat.get("n", 0) == 0 and "terminal_cum_ipd_distribution" not in stat:
                lines.append(f"| {regime} | 0 | 0 | - | - | - | - | - |")
                continue
            d = stat.get("terminal_cum_ipd_distribution", {})
            n_seg = d.get("n", 0)
            n_bars = stat.get("n_bars", 0)
            if n_seg == 0:
                lines.append(f"| {regime} | 0 | {n_bars} | - | - | - | - | - |")
                continue
            lines.append(
                f"| {regime} | {n_seg} | {n_bars} | "
                f"{d['mean']:.3f} | {d['std']:.3f} | "
                f"{d['p05']:.3f} | {d['median']:.3f} | {d['p95']:.3f} |"
            )
        lines.append("")

        # bar IPD distribution (LLN 適用可否)
        lines.append("## bar 単位 IPD 分布 (LLN 適用可否)")
        lines.append("")
        lines.append("| regime | n | mean | std | p05 | median | p95 | skew_proxy |")
        lines.append("|---|---|---|---|---|---|---|---|")
        for regime, stat in result["by_regime"].items():
            d = stat.get("bar_ipd_distribution", {})
            if d.get("n", 0) == 0:
                lines.append(f"| {regime} | 0 | - | - | - | - | - | - |")
                continue
            lines.append(
                f"| {regime} | {d['n']} | "
                f"{d['mean']:.4f} | {d['std']:.4f} | "
                f"{d['p05']:.4f} | {d['median']:.4f} | {d['p95']:.4f} | "
                f"{d['skew_proxy']:.3f} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append(
        "- **terminal_cum_ipd**: closure セグメント1つ毎の終了時点累積 IPD. これがゼロ周りに集中していれば mean reversion が効いている兆候. 大きな std はファットテール."
    )
    lines.append(
        "- **regime 別差**: weekend (R2) と CME daily maintenance (R3) で分布が違えば, 戦略 H1 を regime 別チューニング要."
    )
    lines.append(
        "- **skew_proxy**: (mean - median) / std. 0 なら対称, 大きいほど右肩, 小さいほど左肩."
    )
    lines.append("")
    lines.append("## 次のステップ")
    lines.append("")
    lines.append("1. データ蓄積 (Issue #26 で 1 週間 collect)")
    lines.append(
        "2. n_segments ≥ 30 を満たしたら QQ プロット + Hill 推定で heavy-tail を定量化 (Phase 2)"
    )
    lines.append("3. R2/R3 の差が有意かを Welch t-test で検定")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute_k1(TARGET_SYMBOL)
    md = write_report(result)
    print(f"K1 report: {md}")
    print(json.dumps(result, indent=2, default=str)[:2000])


if __name__ == "__main__":
    main()
