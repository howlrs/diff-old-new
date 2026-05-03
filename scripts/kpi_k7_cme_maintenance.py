"""KPI K7: CMEメンテ時間 (毎日 17-18 ET) IPD 挙動 + メンテ終了時ギャップ分布.

v3 design §3 KPI 7 (Gemini partner 追加).

CMEメンテは年250営業日かける1h 発生 → サンプル蓄積が週末の数十倍.
ここで mean reversion / drift が観測されれば戦略 H3 の根拠.

入力: data/curated/features/*.parquet
出力: docs/kpi/K7.md + K7.json
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
        "min": sv[0],
        "p05": sv[int(0.05 * (n - 1))],
        "median": sv[int(0.5 * (n - 1))],
        "p95": sv[int(0.95 * (n - 1))],
        "max": sv[-1],
    }


def compute_k7(symbol: str = TARGET_SYMBOL) -> dict[str, Any]:
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

    # CME daily maintenance segment (R3) を抽出
    daily = df.filter(pl.col("regime") == Regime.CLOSURE_DAILY.value)

    # セグメント分割: regime が R3 → 他 になる場所で区切り
    seg_change = (pl.col("regime") != pl.col("regime").shift(1)).fill_null(True)
    df = df.with_columns(seg_change.cum_sum().alias("segment_id"))

    daily_with_seg = df.filter(pl.col("regime") == Regime.CLOSURE_DAILY.value)
    if daily_with_seg.is_empty():
        return {
            "symbol": symbol,
            "n_total_bars": int(df.height),
            "n_daily_bars": 0,
            "warning": "no R3 (CME daily maintenance) bars yet",
        }

    # セグメント単位の集計: cumulative IPD, drift_bps (start mid → end mid)
    segments_summary: list[dict[str, Any]] = []
    for seg_id in daily_with_seg["segment_id"].unique().to_list():
        sub = daily_with_seg.filter(pl.col("segment_id") == seg_id)
        if sub.height < 2:
            continue
        first_mid = sub["mid"].drop_nulls().head(1).to_list()
        last_mid = sub["mid"].drop_nulls().tail(1).to_list()
        if not first_mid or not last_mid or first_mid[0] is None or first_mid[0] == 0:
            continue
        drift_bps = (last_mid[0] - first_mid[0]) / first_mid[0] * 10000.0
        cum_ipd = sum(v for v in sub["ipd"].drop_nulls().to_list())
        segments_summary.append(
            {
                "segment_id": int(seg_id),
                "n_bars": sub.height,
                "first_mid": first_mid[0],
                "last_mid": last_mid[0],
                "drift_bps": drift_bps,
                "cum_ipd": cum_ipd,
            }
        )

    drifts = [s["drift_bps"] for s in segments_summary]
    cum_ipds = [s["cum_ipd"] for s in segments_summary]

    # メンテ終了時の next active session 開始ギャップ
    # (R3 → R1 切替点で transition gap を取得)
    transitions = (
        df.with_columns(
            (
                (pl.col("regime") == Regime.ACTIVE.value)
                & (pl.col("regime").shift(1) == Regime.CLOSURE_DAILY.value)
            ).alias("is_post_maint_open")
        )
        .filter(pl.col("is_post_maint_open"))
        .select(["exchange_ts", "mid"])
    )

    return {
        "symbol": symbol,
        "n_total_bars": int(df.height),
        "n_daily_bars": int(daily.height),
        "n_segments": len(segments_summary),
        "drift_bps_distribution": _summarize(drifts),
        "cum_ipd_distribution": _summarize(cum_ipds),
        "n_post_maint_open_transitions": int(transitions.height),
        "samples_first5_segments": segments_summary[:5],
    }


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "K7.md"
    out_json = out_dir / "K7.json"

    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append(f"# KPI K7: CMEメンテ時間 (17-18 ET) IPD 挙動 — `{result['symbol']}`")
    lines.append("")
    lines.append(
        "v3 design §3 KPI 7 (Gemini追加). 毎日 17-18 ET の CME メンテ中の IPD ドリフト + 終了時ギャップ."
    )
    lines.append("")
    lines.append(f"- 全 bars: {result.get('n_total_bars', 0)}")
    lines.append(f"- 日次メンテ bars: {result.get('n_daily_bars', 0)}")
    lines.append(f"- 完了セグメント数: {result.get('n_segments', 0)}")
    lines.append(f"- post-maint open transitions: {result.get('n_post_maint_open_transitions', 0)}")
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")

    if result.get("n_segments", 0) > 0:
        lines.append("## drift_bps 分布 (R3 セグメント単位)")
        d = result["drift_bps_distribution"]
        lines.append(
            f"- n={d['n']}, mean={d['mean']:+.2f} bps, std={d['std']:.2f}, "
            f"median={d['median']:+.2f}, p05={d['p05']:+.2f}, p95={d['p95']:+.2f}"
        )
        lines.append("")
        lines.append("## cumulative IPD 分布")
        d2 = result["cum_ipd_distribution"]
        lines.append(
            f"- n={d2['n']}, mean={d2['mean']:.3f}, std={d2['std']:.3f}, "
            f"median={d2['median']:.3f}, p05={d2['p05']:.3f}, p95={d2['p95']:.3f}"
        )
        lines.append("")
        lines.append("## 先頭5セグメント サンプル")
        lines.append("")
        lines.append("| seg_id | n_bars | first_mid | last_mid | drift_bps | cum_ipd |")
        lines.append("|---|---|---|---|---|---|")
        for s in result.get("samples_first5_segments", []):
            lines.append(
                f"| {s['segment_id']} | {s['n_bars']} | "
                f"{s['first_mid']:.4f} | {s['last_mid']:.4f} | "
                f"{s['drift_bps']:+.2f} | {s['cum_ipd']:.3f} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append(
        "- **drift_bps**: メンテ開始時 mid → 終了時 mid の bps. 0 周辺に集中するなら HL 内部 EMA は安定. heavy-tail なら H3 戦略要慎重."
    )
    lines.append(
        "- **post-maint open transitions**: メンテ終了 (18:00 ET) → active 開始 直後の切替で生じるギャップ取りの素材."
    )
    lines.append("- メンテは年 ~250 営業日 → サンプル蓄積が週末の50倍速い.")
    lines.append("")
    lines.append("## 次のステップ")
    lines.append("")
    lines.append("1. 1週間 collect で R3 セグメントが 5+ 取れるはず")
    lines.append("2. n_segments ≥ 30 で drift_bps 分布の正規性検定 (Shapiro-Wilk + QQプロット)")
    lines.append("3. drift_bps と次 active session 開始 N 分間の return の相関分析")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute_k7(TARGET_SYMBOL)
    md = write_report(result)
    print(f"K7 report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
