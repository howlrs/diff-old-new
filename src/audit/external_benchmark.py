"""Audit-B: 外部突合監査 (Gemini partner 最優先項目).

戦略の根幹である HL Oracle が, 現実の市場と一貫性を保っているかを実証.

スコープ:
1. xyz:SP500 oracle vs SPY (1 分粒度, active session のみ)
   - 相関係数, max abs diff (bps), median diff (bps)
2. BTC oracle vs Binance / Bybit / OKX 中央値
   - HL 公式仕様 weight 3:2:2:1:1:1:1:1 を再現して照合

注意:
- timestamp alignment が最大の罠 (Gemini指摘). 1 分 floor で結合
- yfinance / Yahoo は "personal use only", 個人研究用のみ
- Binance/Bybit/OKX の REST endpoint は公開 (無料)
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import duckdb
import httpx
import polars as pl


@dataclass
class ExternalBenchmark:
    """1 銘柄の外部突合結果."""

    symbol: str
    benchmark_name: str
    n_aligned: int
    correlation: float
    median_diff_bps: float
    p95_abs_diff_bps: float
    max_abs_diff_bps: float
    period_start: str
    period_end: str
    notes: list[str] = field(default_factory=list)


# ----- SPY (Yahoo Finance) ------------------------------------------------


def fetch_spy_minute_bars(start: datetime, end: datetime) -> pl.DataFrame:
    """SPY の 1 分足 OHLC を yfinance で取得.

    Returns:
        DataFrame: ts (Datetime UTC), close (Float64).
        Yahoo の 1 分足は最大 7 日前まで.
    """
    try:
        import yfinance as yf
    except ImportError as exc:
        raise RuntimeError("yfinance not installed (pip install -e '.[audit]')") from exc

    import yfinance as yf

    ticker = yf.Ticker("SPY")
    # yfinance 1m は 7 days 制限
    df = ticker.history(start=start, end=end, interval="1m", auto_adjust=False)
    if df.empty:
        return pl.DataFrame(schema={"ts": pl.Datetime("ns", "UTC"), "close": pl.Float64})

    pdf = df.reset_index().rename(columns={"Datetime": "ts", "Close": "close"})
    if "ts" not in pdf.columns:
        pdf = pdf.rename(columns={pdf.columns[0]: "ts"})
    pl_df = pl.from_pandas(pdf[["ts", "close"]])
    # tz convert to UTC
    pl_df = pl_df.with_columns(pl.col("ts").dt.convert_time_zone("UTC").alias("ts"))
    return pl_df


def align_minute_bars(
    hl_oracle: pl.DataFrame,
    bench: pl.DataFrame,
) -> pl.DataFrame:
    """HL の oracle 系列と外部 bench を 1 分粒度で結合.

    入力:
        hl_oracle: ts, oracle_px
        bench: ts, close
    出力:
        ts, hl_px, bench_px, diff_bps
    """
    if hl_oracle.is_empty() or bench.is_empty():
        return pl.DataFrame(
            schema={
                "ts": pl.Datetime,
                "hl_px": pl.Float64,
                "bench_px": pl.Float64,
                "diff_bps": pl.Float64,
            }
        )

    def _strip_tz(df: pl.DataFrame, col: str) -> pl.DataFrame:
        """tz-aware/naive を統一 (naive UTC) して 1m floor を取る."""
        s = df[col]
        # tz-aware なら convert UTC → strip
        if isinstance(s.dtype, pl.Datetime) and s.dtype.time_zone is not None:
            return df.with_columns(
                pl.col(col).dt.convert_time_zone("UTC").dt.replace_time_zone(None).alias(col)
            )
        return df

    hl_oracle = _strip_tz(hl_oracle, "ts")
    bench = _strip_tz(bench, "ts")

    # 1 分 floor
    hl_min = (
        hl_oracle.with_columns(pl.col("ts").dt.truncate("1m").alias("ts_min"))
        .group_by("ts_min")
        .agg(pl.col("oracle_px").mean().alias("hl_px"))
    )
    bench_min = (
        bench.with_columns(pl.col("ts").dt.truncate("1m").alias("ts_min"))
        .group_by("ts_min")
        .agg(pl.col("close").mean().alias("bench_px"))
    )

    joined = hl_min.join(bench_min, on="ts_min", how="inner").rename({"ts_min": "ts"})
    if joined.is_empty():
        return joined
    return joined.with_columns(
        ((pl.col("hl_px") - pl.col("bench_px")) / pl.col("bench_px") * 10000.0).alias("diff_bps")
    ).sort("ts")


def _summarize_bench(symbol: str, name: str, aligned: pl.DataFrame) -> ExternalBenchmark:
    if aligned.is_empty():
        return ExternalBenchmark(
            symbol=symbol,
            benchmark_name=name,
            n_aligned=0,
            correlation=float("nan"),
            median_diff_bps=float("nan"),
            p95_abs_diff_bps=float("nan"),
            max_abs_diff_bps=float("nan"),
            period_start="-",
            period_end="-",
            notes=["no aligned data"],
        )
    diffs = aligned["diff_bps"].drop_nulls().to_list()
    abs_diffs = [abs(d) for d in diffs]
    sv = sorted(abs_diffs)
    median = aligned["diff_bps"].median()
    p95 = sv[int(0.95 * (len(sv) - 1))] if sv else float("nan")
    mx = max(abs_diffs) if abs_diffs else float("nan")
    if "hl_px" in aligned.columns and aligned.height >= 3:
        corr_df = aligned.select(["hl_px", "bench_px"]).drop_nulls()
        if corr_df.height >= 3 and corr_df["hl_px"].std() > 0 and corr_df["bench_px"].std() > 0:
            corr = float(corr_df.corr().to_numpy()[0, 1]) if hasattr(corr_df, "corr") else 0.0
        else:
            corr = float("nan")
    else:
        corr = float("nan")

    return ExternalBenchmark(
        symbol=symbol,
        benchmark_name=name,
        n_aligned=aligned.height,
        correlation=corr,
        median_diff_bps=float(median) if median is not None else float("nan"),
        p95_abs_diff_bps=p95,
        max_abs_diff_bps=mx,
        period_start=str(aligned["ts"].min()),
        period_end=str(aligned["ts"].max()),
    )


# ----- BTC (Binance / Bybit / OKX) ---------------------------------------


HL_BTC_CEX_WEIGHTS: dict[str, float] = {
    # HL 公式仕様の weight: Binance, OKX, Bybit, Kraken, Kucoin, Gate IO, MEXC
    # = 3, 2, 2, 1, 1, 1, 1
    # 本 audit ではユーザー説明のとおり Binance / Bybit / OKX のみで比較
    "binance": 3.0,
    "bybit": 2.0,
    "okx": 2.0,
}


async def fetch_binance_klines(
    symbol: str = "BTCUSDT", interval: str = "1m", limit: int = 1000
) -> pl.DataFrame:
    url = "https://api.binance.com/api/v3/klines"
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(
            url, params={"symbol": symbol, "interval": interval, "limit": limit}
        )
        resp.raise_for_status()
        rows = resp.json()
    if not rows:
        return pl.DataFrame()
    df = pl.DataFrame(
        {
            "ts": [datetime.fromtimestamp(r[0] / 1000, tz=UTC) for r in rows],
            "close": [float(r[4]) for r in rows],
        }
    )
    return df


async def fetch_bybit_klines(
    symbol: str = "BTCUSDT", interval: str = "1", limit: int = 1000
) -> pl.DataFrame:
    url = "https://api.bybit.com/v5/market/kline"
    params = {"category": "spot", "symbol": symbol, "interval": interval, "limit": limit}
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(url, params=params)
        resp.raise_for_status()
        data = resp.json()
    rows = data.get("result", {}).get("list", [])
    if not rows:
        return pl.DataFrame()
    # Bybit returns descending order
    rows = list(reversed(rows))
    df = pl.DataFrame(
        {
            "ts": [datetime.fromtimestamp(int(r[0]) / 1000, tz=UTC) for r in rows],
            "close": [float(r[4]) for r in rows],
        }
    )
    return df


async def fetch_okx_klines(
    symbol: str = "BTC-USDT", interval: str = "1m", limit: int = 300
) -> pl.DataFrame:
    url = "https://www.okx.com/api/v5/market/candles"
    params = {"instId": symbol, "bar": interval, "limit": str(limit)}
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(url, params=params)
        resp.raise_for_status()
        data = resp.json()
    rows = data.get("data", [])
    if not rows:
        return pl.DataFrame()
    rows = list(reversed(rows))
    df = pl.DataFrame(
        {
            "ts": [datetime.fromtimestamp(int(r[0]) / 1000, tz=UTC) for r in rows],
            "close": [float(r[4]) for r in rows],
        }
    )
    return df


def weighted_median_close(
    sources: dict[str, pl.DataFrame], weights: dict[str, float]
) -> pl.DataFrame:
    """三市場の close を weight 付き中央値で集約 (1 分粒度).

    HL 公式仕様 (Binance:3, OKX:2, Bybit:2) を再現. Polars で各 1m bin に対して
    weighted median を計算する.
    """
    if not sources:
        return pl.DataFrame()
    # 1m floor
    minute_dfs = []
    for name, df in sources.items():
        if df.is_empty():
            continue
        w = weights.get(name, 1.0)
        df_min = (
            df.with_columns(pl.col("ts").dt.truncate("1m").alias("ts_min"))
            .group_by("ts_min")
            .agg(pl.col("close").mean().alias(f"{name}_close"))
            .with_columns(pl.lit(w).alias(f"{name}_w"))
        )
        minute_dfs.append(df_min)
    if not minute_dfs:
        return pl.DataFrame()

    base = minute_dfs[0]
    for df in minute_dfs[1:]:
        base = base.join(df, on="ts_min", how="full", coalesce=True)

    # weighted median: weight repetition で median を取る簡易版
    out_rows: list[dict] = []
    for row in base.iter_rows(named=True):
        pairs: list[tuple[float, float]] = []
        for name in sources:
            close = row.get(f"{name}_close")
            w = row.get(f"{name}_w")
            if close is None or w is None:
                continue
            pairs.append((float(close), float(w)))
        if not pairs:
            continue
        # 重み付き median (weight 整数倍に展開して中央値)
        pairs.sort(key=lambda x: x[0])
        total_w = sum(w for _, w in pairs)
        cum = 0.0
        median = pairs[0][0]
        for px, w in pairs:
            cum += w
            if cum >= total_w / 2:
                median = px
                break
        out_rows.append({"ts": row["ts_min"], "close": median})
    if not out_rows:
        return pl.DataFrame()
    return pl.DataFrame(out_rows).sort("ts")


# ----- HL data loader ----------------------------------------------------


def load_hl_oracle(raw_root: Path, symbol: str) -> pl.DataFrame:
    """asset_ctxs から oracle 時系列を取得 (1 分粒度に丸めない, raw poll_ts)."""
    glob = str(raw_root / "asset_ctxs" / "**" / "*.parquet")
    from glob import glob as _glob

    if not _glob(glob, recursive=True):
        return pl.DataFrame()
    con = duckdb.connect(":memory:")
    df = con.execute(
        f"SELECT poll_ts AS ts, oracle_px FROM read_parquet('{glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' AND oracle_px IS NOT NULL ORDER BY poll_ts"
    ).pl()
    if df.is_empty():
        return df
    # tz UTC に統一
    return df.with_columns(pl.col("ts").dt.convert_time_zone("UTC").alias("ts"))


# ----- runner ------------------------------------------------------------


async def run_crypto_audit(
    raw_root: Path, hl_symbol: str, cex_pairs: dict[str, str]
) -> ExternalBenchmark:
    """汎用 crypto audit. cex_pairs は {取引所名: ペア記号}."""
    hl = load_hl_oracle(raw_root, hl_symbol)
    if hl.is_empty():
        return _summarize_bench(hl_symbol, "binance+okx+bybit weighted median", pl.DataFrame())

    sources: dict[str, pl.DataFrame] = {}
    if "binance" in cex_pairs:
        sources["binance"] = await fetch_binance_klines(cex_pairs["binance"])
    if "bybit" in cex_pairs:
        sources["bybit"] = await fetch_bybit_klines(cex_pairs["bybit"])
    if "okx" in cex_pairs:
        sources["okx"] = await fetch_okx_klines(cex_pairs["okx"])

    bench = weighted_median_close(sources, HL_BTC_CEX_WEIGHTS)
    if bench.is_empty():
        return _summarize_bench(hl_symbol, "binance+okx+bybit weighted median", pl.DataFrame())

    aligned = align_minute_bars(hl, bench.rename({"close": "close"}))
    return _summarize_bench(hl_symbol, "binance+okx+bybit weighted median", aligned)


async def run_btc_audit(raw_root: Path) -> ExternalBenchmark:
    return await run_crypto_audit(
        raw_root,
        "BTC",
        {"binance": "BTCUSDT", "bybit": "BTCUSDT", "okx": "BTC-USDT"},
    )


async def run_eth_audit(raw_root: Path) -> ExternalBenchmark:
    return await run_crypto_audit(
        raw_root,
        "ETH",
        {"binance": "ETHUSDT", "bybit": "ETHUSDT", "okx": "ETH-USDT"},
    )


def run_spy_audit(raw_root: Path) -> ExternalBenchmark:
    hl = load_hl_oracle(raw_root, "xyz:SP500")
    if hl.is_empty():
        return _summarize_bench("xyz:SP500", "SPY (yfinance)", pl.DataFrame())

    period_min = hl["ts"].min()
    period_max = hl["ts"].max()
    if period_min is None or period_max is None:
        return _summarize_bench("xyz:SP500", "SPY (yfinance)", pl.DataFrame())
    start = (period_min - timedelta(minutes=5)).replace(tzinfo=UTC)
    end = (period_max + timedelta(minutes=5)).replace(tzinfo=UTC)
    try:
        spy = fetch_spy_minute_bars(start, end)
    except Exception as exc:
        bench = _summarize_bench("xyz:SP500", "SPY (yfinance)", pl.DataFrame())
        bench.notes.append(f"yfinance error: {exc}")
        return bench

    aligned = align_minute_bars(hl, spy)
    return _summarize_bench("xyz:SP500", "SPY (yfinance)", aligned)


@dataclass
class ExternalAuditReport:
    benchmarks: list[ExternalBenchmark] = field(default_factory=list)


async def run_external_audit_async(raw_root: Path) -> ExternalAuditReport:
    btc = await run_btc_audit(raw_root)
    eth = await run_eth_audit(raw_root)
    spy = run_spy_audit(raw_root)
    # xyz:XYZ100 は SPY 同様 yfinance に頼るが NDX (^NDX or QQQ ETF) を使う必要があり別実装
    return ExternalAuditReport(benchmarks=[btc, eth, spy])


def render_markdown(report: ExternalAuditReport) -> str:
    lines: list[str] = []
    lines.append("# Audit-B: external benchmark report")
    lines.append("")
    lines.append("Gemini partner 最優先項目: HL Oracle が外部市場と一致しているかの実証.")
    lines.append("")
    lines.append(
        "| symbol | benchmark | n_aligned | corr | median diff bps | p95 abs diff bps | max abs diff bps |"
    )
    lines.append("|---|---|---|---|---|---|---|")
    for b in report.benchmarks:
        corr_s = f"{b.correlation:+.4f}" if not _is_nan(b.correlation) else "-"
        med = f"{b.median_diff_bps:+.2f}" if not _is_nan(b.median_diff_bps) else "-"
        p95 = f"{b.p95_abs_diff_bps:.2f}" if not _is_nan(b.p95_abs_diff_bps) else "-"
        mx = f"{b.max_abs_diff_bps:.2f}" if not _is_nan(b.max_abs_diff_bps) else "-"
        lines.append(
            f"| {b.symbol} | {b.benchmark_name} | {b.n_aligned} | {corr_s} | {med} | {p95} | {mx} |"
        )
    lines.append("")
    for b in report.benchmarks:
        if b.notes:
            lines.append(f"### {b.symbol} notes")
            for n in b.notes:
                lines.append(f"- {n}")
            lines.append("")
    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append("- **corr ≈ 1.0**: HL Oracle が外部市場と整合している (健全)")
    lines.append(
        "- **median diff bps**: 0 周辺なら系統バイアス無し. ±10 bps 程度は active 中の遅延と整合"
    )
    lines.append(
        "- **p95 abs diff bps**: closure 中はHL内部 EMA で乖離するので大きくて正常. active のみで filter すると本物の市場差が見える"
    )
    lines.append("- **max abs diff bps**: 一時的な outlier (relayer 停止, BTC 急変動 etc)")
    lines.append("")
    lines.append("## 本 audit の限界")
    lines.append("")
    lines.append("- yfinance の SPY 1分足は 7 日前まで. 長期運用では Polygon 等への切替を検討")
    lines.append(
        "- weighted median は HL の正式 oracle 計算を完全再現していない (validator stake-weight 部分は不明)"
    )
    lines.append("- 1 分 floor アライメント. ms 級ズレは検出不可")
    return "\n".join(lines)


def _is_nan(x: Any) -> bool:
    try:
        return x != x  # NaN check
    except Exception:
        return False


# Sync wrapper for convenience
def run_external_audit(raw_root: Path) -> ExternalAuditReport:
    return asyncio.run(run_external_audit_async(raw_root))
