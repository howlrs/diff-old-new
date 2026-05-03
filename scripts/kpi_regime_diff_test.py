"""Phase 2 KPI: regime 間の IPD ドリフト有意差検定 (Issue #39).

Welch's t-test (不等分散) で R2 (週末) / R3 (CMEメンテ) / R4 (祝日) の
IPD 分布が統計的に異なるかを検定. p<0.05 なら regime 別チューニング要.

入力: data/curated/features/*.parquet
出力: docs/kpi/regime_diff.{md,json}
"""

from __future__ import annotations

import json
import sys
from itertools import combinations
from pathlib import Path
from typing import Any

import polars as pl

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from _common import safe_read_parquet_glob  # noqa: E402

from src.config import load_config  # noqa: E402
from src.l2_features.distribution import welch_t_test  # noqa: E402
from src.l2_features.regime import Regime  # noqa: E402

TARGET_SYMBOLS = ["xyz:SP500", "xyz:XYZ100", "BTC", "ETH"]
TARGET_REGIMES = [
    Regime.CLOSURE_WEEKEND.value,
    Regime.CLOSURE_DAILY.value,
    Regime.CLOSURE_HOLIDAY.value,
]


def compute() -> dict[str, Any]:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    glob = str(cfg.storage.curated_data_root / "features" / "**" / "*.parquet")
    df = safe_read_parquet_glob(glob)
    if df.is_empty():
        return {"warning": "no curated features yet"}

    by_symbol: dict[str, dict[str, Any]] = {}
    for sym in TARGET_SYMBOLS:
        sub_sym = df.filter(pl.col("symbol") == sym)
        if sub_sym.is_empty():
            by_symbol[sym] = {"warning": "no data"}
            continue
        regime_samples: dict[str, list[float]] = {}
        for regime in TARGET_REGIMES:
            ipd = sub_sym.filter(pl.col("regime") == regime)["ipd"].drop_nulls()
            regime_samples[regime] = [float(v) for v in ipd.to_list()]

        pairs: dict[str, dict[str, Any]] = {}
        for r_a, r_b in combinations(TARGET_REGIMES, 2):
            a = regime_samples[r_a]
            b = regime_samples[r_b]
            if len(a) < 2 or len(b) < 2:
                pairs[f"{r_a}_vs_{r_b}"] = {
                    "n_a": len(a),
                    "n_b": len(b),
                    "warning": "insufficient samples",
                }
                continue
            t, p = welch_t_test(a, b)
            pairs[f"{r_a}_vs_{r_b}"] = {
                "n_a": len(a),
                "n_b": len(b),
                "t_stat": t,
                "p_value": p,
                "is_significant_005": p < 0.05,
            }
        by_symbol[sym] = pairs

    return {"by_symbol": by_symbol}


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "regime_diff.md"
    out_json = out_dir / "regime_diff.json"
    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append("# Phase 2 KPI: regime 間 IPD ドリフト有意差検定 (Welch's t-test)")
    lines.append("")
    lines.append(
        "v3 design §3 K1 の延長. R2 (週末) / R3 (CMEメンテ) / R4 (祝日) の IPD bar 分布が "
        "統計的に異なるかを検定. p<0.05 なら戦略 H1 を regime 別にチューニング."
    )
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")
        out_md.write_text("\n".join(lines), encoding="utf-8")
        return out_md

    by_symbol = result.get("by_symbol", {})
    for sym, pairs in by_symbol.items():
        if isinstance(pairs, dict) and pairs.get("warning"):
            lines.append(f"## {sym}: {pairs['warning']}")
            lines.append("")
            continue
        lines.append(f"## {sym}")
        lines.append("")
        lines.append("| pair | n_a | n_b | t_stat | p_value | significant (p<0.05) |")
        lines.append("|---|---|---|---|---|---|")
        for pair, stat in pairs.items():
            if stat.get("warning"):
                lines.append(
                    f"| {pair} | {stat.get('n_a', 0)} | {stat.get('n_b', 0)} | - | - | (warn) |"
                )
                continue
            sig = "**YES**" if stat["is_significant_005"] else "no"
            lines.append(
                f"| {pair} | {stat['n_a']} | {stat['n_b']} | "
                f"{stat['t_stat']:+.3f} | {stat['p_value']:.4f} | {sig} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append("- **p < 0.05** = 2 regime の IPD 平均が有意に異なる")
    lines.append("- 全 pair で有意 → regime 別パラメータ必須")
    lines.append("- 全 pair で非有意 → 共通パラメータで H1 を統一できる")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute()
    md = write_report(result)
    print(f"regime_diff report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
