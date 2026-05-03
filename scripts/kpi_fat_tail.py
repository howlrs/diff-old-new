"""Phase 2 KPI: ファットテール定量化 (Issue #38).

regime 別に IPD bar 値の分布形状を解析:
- Hill 推定で tail index alpha
- Shapiro-Wilk / D'Agostino-Pearson 正規性検定
- skewness / kurtosis

入力: data/curated/features/*.parquet
出力: docs/kpi/fat_tail.{md,json}
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import polars as pl

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from _common import safe_read_parquet_glob  # noqa: E402

from src.config import load_config  # noqa: E402
from src.l2_features.distribution import analyze_distribution  # noqa: E402
from src.l2_features.regime import Regime  # noqa: E402

TARGET_SYMBOLS = ["xyz:SP500", "xyz:XYZ100", "BTC", "ETH"]
ALL_REGIMES = [
    Regime.ACTIVE.value,
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
        per_regime: dict[str, dict[str, Any]] = {}
        for regime in ALL_REGIMES:
            sub = sub_sym.filter(pl.col("regime") == regime)["ipd"].drop_nulls()
            values = [float(v) for v in sub.to_list()]
            stats = analyze_distribution(values)
            per_regime[regime] = {
                "n": stats.n,
                "mean": stats.mean,
                "std": stats.std,
                "skewness": stats.skewness,
                "kurtosis": stats.kurtosis,
                "hill_alpha": stats.hill_alpha,
                "shapiro_pvalue": stats.shapiro_pvalue,
                "is_heavy_tail": stats.is_heavy_tail,
            }
        by_symbol[sym] = per_regime
    return {"by_symbol": by_symbol}


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "fat_tail.md"
    out_json = out_dir / "fat_tail.json"
    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append("# Phase 2 KPI: 分布のファットテール定量化")
    lines.append("")
    lines.append(
        "v3 design §3 K10/K11 の前段. regime 別 IPD 分布の正規性を Hill 推定 + Shapiro-Wilk で検定."
    )
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")
        out_md.write_text("\n".join(lines), encoding="utf-8")
        return out_md

    by_symbol = result.get("by_symbol", {})
    for sym, per_regime in by_symbol.items():
        if isinstance(per_regime, dict) and per_regime.get("warning"):
            lines.append(f"## {sym}: {per_regime['warning']}")
            lines.append("")
            continue
        lines.append(f"## {sym}")
        lines.append("")
        lines.append("| regime | n | mean | std | skew | kurt | hill_alpha | shapiro_p | heavy? |")
        lines.append("|---|---|---|---|---|---|---|---|---|")
        for regime, stat in per_regime.items():
            if stat.get("n", 0) < 8:
                lines.append(f"| {regime} | {stat.get('n', 0)} | - | - | - | - | - | - | - |")
                continue
            ha = stat.get("hill_alpha")
            sp = stat.get("shapiro_pvalue")
            ha_s = f"{ha:.2f}" if ha is not None else "-"
            sp_s = f"{sp:.4f}" if sp is not None else "-"
            heavy = "YES" if stat.get("is_heavy_tail") else "no"
            lines.append(
                f"| {regime} | {stat['n']} | {stat['mean']:+.3f} | {stat['std']:.3f} | "
                f"{stat['skewness']:+.2f} | {stat['kurtosis']:+.2f} | {ha_s} | {sp_s} | {heavy} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append("- **kurt > 3**: 正規分布よりピーク鋭く, 裾が厚い (heavy-tail)")
    lines.append("- **hill_alpha < 4**: tail がべき乗則に近い heavy-tail")
    lines.append("- **shapiro_pvalue < 0.05**: 正規性棄却 (CLT 収束遅い)")
    lines.append(
        "- heavy=YES の regime は LLN/CLT で mean+/-k*sigma/sqrt(N) の信頼区間が信用できない"
    )
    lines.append("- Phase 2 採否判定: heavy-tail 銘柄/regime は分布 tail を直接狙う戦略にする")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute()
    md = write_report(result)
    print(f"fat_tail report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
