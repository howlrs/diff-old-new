"""KPI K8: 週末 BTC/ETH ボラ vs TradFi 銘柄 IPD 相関 (Issue #24, Gemini partner 追加).

仮説 H2: closure 中, HL TradFi perp は BTC/ETH の動きに引きずられる
(Crypto Native 相関). 清算カスケードでも検証.

入力: data/curated/features/*.parquet (regime/IPD)
     + data/raw/l2book で BTC mid 系列
出力: docs/kpi/K8.{md,json}
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path
from typing import Any

import duckdb
import numpy as np
import polars as pl

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

from src.config import load_config  # noqa: E402
from src.l2_features.regime import Regime  # noqa: E402

TARGET_TRAD = "xyz:SP500"
CRYPTO_BENCHMARKS = ["BTC", "ETH"]
ROLLING_WINDOW_BARS = 60  # 5min x 60 = 5h equivalent (Phase 1 仮値)
CLOSURE_REGIMES = [
    Regime.CLOSURE_WEEKEND.value,
    Regime.CLOSURE_DAILY.value,
    Regime.CLOSURE_HOLIDAY.value,
]


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
    }


def _read(glob: str) -> pl.DataFrame:
    con = duckdb.connect(":memory:")
    arrow_tbl = con.execute(f"SELECT * FROM read_parquet('{glob}', union_by_name=true)").arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()
    return df


def compute_k8() -> dict[str, Any]:
    cfg = load_config(
        [
            REPO_ROOT / "config" / "default.yaml",
            REPO_ROOT / "config" / "local.yaml",
        ]
    )
    feat_glob = str(cfg.storage.curated_data_root / "features" / "**" / "*.parquet")
    df = _read(feat_glob)
    if df.is_empty():
        return {"warning": "no features"}

    df = df.sort("exchange_ts")
    by_crypto: dict[str, dict[str, Any]] = {}

    for crypto in CRYPTO_BENCHMARKS:
        # closure regime のみで TARGET_TRAD と crypto を結合
        trad = (
            df.filter((pl.col("symbol") == TARGET_TRAD) & (pl.col("regime").is_in(CLOSURE_REGIMES)))
            .select(["exchange_ts", "ipd", "mid", "regime"])
            .rename({"ipd": "trad_ipd", "mid": "trad_mid", "regime": "trad_regime"})
        )
        cry = (
            df.filter(pl.col("symbol") == crypto)
            .select(["exchange_ts", "mid"])
            .rename({"mid": "crypto_mid"})
        )
        if trad.is_empty() or cry.is_empty():
            by_crypto[crypto] = {"warning": "insufficient data"}
            continue
        joined = trad.join_asof(cry, on="exchange_ts", strategy="backward")
        # crypto returns + trad ipd の同時刻系列
        joined = joined.with_columns(
            (pl.col("crypto_mid").pct_change()).alias("crypto_ret"),
        )
        joined = joined.drop_nulls(subset=["trad_ipd", "crypto_ret"])
        if joined.height < 30:
            by_crypto[crypto] = {
                "n": joined.height,
                "warning": "insufficient overlap",
            }
            continue
        trad_ipd = np.array(joined["trad_ipd"].to_list(), dtype=float)
        crypto_ret = np.array(joined["crypto_ret"].to_list(), dtype=float)
        # サンプル相関
        if np.std(trad_ipd) > 0 and np.std(crypto_ret) > 0:
            corr = float(np.corrcoef(trad_ipd, crypto_ret)[0, 1])
        else:
            corr = float("nan")
        # ローリング相関 (window)
        rolling_corrs: list[float] = []
        if joined.height >= ROLLING_WINDOW_BARS:
            for i in range(ROLLING_WINDOW_BARS, joined.height):
                a = trad_ipd[i - ROLLING_WINDOW_BARS : i]
                b = crypto_ret[i - ROLLING_WINDOW_BARS : i]
                if np.std(a) > 0 and np.std(b) > 0:
                    rolling_corrs.append(float(np.corrcoef(a, b)[0, 1]))

        by_crypto[crypto] = {
            "n": joined.height,
            "overall_corr": corr,
            "rolling_corr_distribution": _summarize(rolling_corrs),
        }

    return {
        "target_tradfi": TARGET_TRAD,
        "by_crypto": by_crypto,
    }


def write_report(result: dict[str, Any]) -> Path:
    out_dir = REPO_ROOT / "docs" / "kpi"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_md = out_dir / "K8.md"
    out_json = out_dir / "K8.json"
    out_json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")

    lines: list[str] = []
    lines.append(f"# KPI K8: closure 中 BTC/ETH vs `{result.get('target_tradfi', '?')}` IPD 相関")
    lines.append("")
    lines.append(
        "v3 design §3 KPI 8. 仮説 H2 (Crypto Native 相関) の検証. closure regime のみで分析."
    )
    lines.append("")
    if result.get("warning"):
        lines.append(f"> ⚠ **WARNING**: {result['warning']}")
        lines.append("")

    by = result.get("by_crypto", {})
    if by:
        lines.append("## 全体 sample 相関")
        lines.append("")
        lines.append("| crypto | n | overall_corr | rolling median | rolling p05 | p95 |")
        lines.append("|---|---|---|---|---|---|")
        for c, stat in by.items():
            if stat.get("warning"):
                lines.append(
                    f"| {c} | {stat.get('n', 0)} | - (warn: {stat['warning']}) | - | - | - |"
                )
                continue
            d = stat.get("rolling_corr_distribution", {})
            r_med = d.get("median", "-") if d.get("n", 0) > 0 else "-"
            r_p05 = d.get("p05", "-") if d.get("n", 0) > 0 else "-"
            r_p95 = d.get("p95", "-") if d.get("n", 0) > 0 else "-"
            r_med_s = f"{r_med:+.3f}" if isinstance(r_med, (int, float)) else r_med
            r_p05_s = f"{r_p05:+.3f}" if isinstance(r_p05, (int, float)) else r_p05
            r_p95_s = f"{r_p95:+.3f}" if isinstance(r_p95, (int, float)) else r_p95
            lines.append(
                f"| {c} | {stat.get('n', 0)} | {stat.get('overall_corr', 0):+.3f} | "
                f"{r_med_s} | {r_p05_s} | {r_p95_s} |"
            )
        lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append(
        "- **overall_corr**: closure 中の TradFi IPD と Crypto return の同時刻相関. >0.2 で連動仮説支持."
    )
    lines.append(
        "- **rolling p95 が高い** → 一時的に強相関する瞬間がある = 清算カスケード等のイベント."
    )
    lines.append("- **時間帯別 break down** は Phase 2 で追加 (eg. アジア時間 vs NY時間).")
    lines.append("")
    lines.append("## 次のステップ")
    lines.append("")
    lines.append("1. 1 週間で BTC 急変動イベント (>2% in 1h) を抽出, その瞬間の TradFi IPD を観測")
    lines.append("2. lead-lag 分析: BTC 動きが N 秒先行か, 同時か")
    lines.append("3. 清算データ (HL の liquidations) を入手して連鎖を可視化 (Phase 2)")

    out_md.write_text("\n".join(lines), encoding="utf-8")
    return out_md


def main() -> None:
    result = compute_k8()
    md = write_report(result)
    print(f"K8 report: {md}")
    print(json.dumps(result, indent=2, default=str)[:1500])


if __name__ == "__main__":
    main()
