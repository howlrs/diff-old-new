"""KPI K9: 板の Resilience 分布 (Issue #25, Gemini partner 追加).

CMEメンテ・週末・active 各 regime 別に, 大口Taker後の板回復時間を集計.
これが Capacity (運用上限) を直接決定する.

入力: data/raw/{l2book,trades}/*.parquet
出力: docs/kpi/K9.{md,json}
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
from src.l2_features.regime import classify_regime  # noqa: E402
from src.l2_features.resilience import compute_resilience_distribution  # noqa: E402

TARGET_SYMBOLS = ["xyz:SP500", "xyz:XYZ100", "BTC", "ETH"]


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


def _read_parquet(glob: str) -> pl.DataFrame:
    con = duckdb.connect(":memory:")
    arrow_tbl = con.execute(f"SELECT * FROM read_parquet('{glob}', union_by_name=true)").arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()
    return df


def compute_k9() -> dict[str, Any]:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    raw = cfg.storage.raw_data_root
    trades_glob = str(raw / "trades" / "**" / "*.parquet")
    l2_glob = str(raw / "l2book" / "**" / "*.parquet")

    trades = _read_parquet(trades_glob)
    l2 = _read_parquet(l2_glob)

    if trades.is_empty() or l2.is_empty():
        return {"warning": "no trades or l2 data"}

    trades = trades.filter(pl.col("symbol").is_in(TARGET_SYMBOLS))
    l2 = l2.filter(pl.col("symbol").is_in(TARGET_SYMBOLS))

    by_symbol: dict[str, dict[str, Any]] = {}
    for sym in TARGET_SYMBOLS:
        events = compute_resilience_distribution(
            trades.filter(pl.col("symbol") == sym),
            l2.filter(pl.col("symbol") == sym),
        )
        if not events:
            by_symbol[sym] = {"n": 0}
            continue

        # regime 分類
        for ev in events:
            ev_dict = ev.__dict__
            ev_dict["regime"] = (
                classify_regime(ev.trade_ts, cfg.regime).value if ev.trade_ts else "unknown"
            )

        recovered = [
            float(ev.recovery_sec)
            for ev in events
            if ev.is_recovered and ev.recovery_sec is not None
        ]
        sizes = [ev.trade_size_usd for ev in events]

        by_regime: dict[str, dict[str, Any]] = {}
        for regime in {ev.__dict__["regime"] for ev in events}:
            regime_recs = [
                float(ev.recovery_sec)
                for ev in events
                if ev.__dict__["regime"] == regime
                and ev.is_recovered
                and ev.recovery_sec is not None
            ]
            by_regime[regime] = {
                "n_events": sum(1 for ev in events if ev.__dict__["regime"] == regime),
                "recovery_sec_distribution": _summarize(regime_recs),
            }

        by_symbol[sym] = {
            "n_events": len(events),
            "n_recovered": len(recovered),
            "recovery_rate": len(recovered) / len(events) if events else 0.0,
            "recovery_sec_distribution": _summarize(recovered),
            "size_usd_distribution": _summarize(sizes),
            "by_regime": by_regime,
        }

    return {
        "n_total_events": sum(v.get("n_events", 0) for v in by_symbol.values()),
        "by_symbol": by_symbol,
    }


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "K9.md"
    out_json = out_dir / "K9.json"
    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append("# KPI K9: 板の Resilience (大口Taker後の回復時間)")
    lines.append("")
    lines.append("v3 design §3 KPI 9 (Gemini追加). Capacity を直接決定する指標.")
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")

    lines.append(f"全イベント数: {result.get('n_total_events', 0)}")
    lines.append("")
    by_symbol = result.get("by_symbol", {})
    if by_symbol:
        lines.append("## symbol 別 サマリ")
        lines.append("")
        lines.append("| symbol | n_events | recovery_rate | recovery_sec median | p95 |")
        lines.append("|---|---|---|---|---|")
        for sym, stat in by_symbol.items():
            n = stat.get("n_events", 0)
            if n == 0:
                lines.append(f"| {sym} | 0 | - | - | - |")
                continue
            rate = stat.get("recovery_rate", 0)
            d = stat.get("recovery_sec_distribution", {})
            if d.get("n", 0) == 0:
                lines.append(f"| {sym} | {n} | {rate * 100:.1f}% | timeout | timeout |")
                continue
            lines.append(
                f"| {sym} | {n} | {rate * 100:.1f}% | {d['median']:.2f}s | {d['p95']:.2f}s |"
            )
        lines.append("")

        lines.append("## regime 別")
        lines.append("")
        for sym, stat in by_symbol.items():
            if "by_regime" not in stat:
                continue
            lines.append(f"### {sym}")
            lines.append("")
            lines.append("| regime | n_events | recovery_sec median | p95 |")
            lines.append("|---|---|---|---|")
            for regime, rs in stat.get("by_regime", {}).items():
                d = rs.get("recovery_sec_distribution", {})
                if d.get("n", 0) == 0:
                    lines.append(f"| {regime} | {rs.get('n_events', 0)} | - | - |")
                    continue
                lines.append(
                    f"| {regime} | {rs['n_events']} | {d['median']:.2f}s | {d['p95']:.2f}s |"
                )
            lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append(
        "- **recovery_sec**: 大口Taker直後 spread 拡大 → 元水準復帰までの秒数. 短いほど Capacity 大."
    )
    lines.append("- **recovery_rate**: timeout (5分) 内に回復した割合. 100% に近いと健全.")
    lines.append("- **regime 別差**: closure では Capacity が小さい (薄い板) と予想.")
    lines.append("")
    lines.append("## 次のステップ")
    lines.append("")
    lines.append("1. 1 週間 collect 後に再実行 (現状はデータ薄)")
    lines.append("2. 戦略 H1/H3 のサイジングを各 regime の p95 recovery_sec で制限")
    lines.append("3. 板薄銘柄 (XYZ100) と 板厚銘柄 (BTC) の比較で Capacity 上限を可視化")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute_k9()
    md = write_report(result)
    print(f"K9 report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
