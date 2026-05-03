"""パフォーマンス指標: Sharpe / Sortino / Max DD / Hit Rate / Profit Factor.

GUI ダッシュボードの上段で使う. 純粋数値計算なので marimo / altair に依存しない.
"""

from __future__ import annotations

import math
from dataclasses import dataclass
from datetime import datetime, timedelta

import polars as pl


@dataclass
class PerfStats:
    """戦略 backtest の performance 統計サマリ."""

    n_trades: int
    total_pnl_usd: float
    total_cost_usd: float
    sharpe_annualized: float
    sharpe_se: float  # standard error
    sharpe_ci_low: float
    sharpe_ci_high: float
    sortino_annualized: float
    calmar: float
    max_drawdown_pct: float
    hit_rate: float
    profit_factor: float
    expectancy_bps: float
    mean_bps: float
    std_bps: float

    @classmethod
    def empty(cls) -> PerfStats:
        return cls(
            n_trades=0,
            total_pnl_usd=0.0,
            total_cost_usd=0.0,
            sharpe_annualized=0.0,
            sharpe_se=0.0,
            sharpe_ci_low=0.0,
            sharpe_ci_high=0.0,
            sortino_annualized=0.0,
            calmar=0.0,
            max_drawdown_pct=0.0,
            hit_rate=0.0,
            profit_factor=0.0,
            expectancy_bps=0.0,
            mean_bps=0.0,
            std_bps=0.0,
        )


def _annualization_factor(trades_df: pl.DataFrame, fallback_n_per_year: int) -> float:
    """trades の時間軸から年間試行回数を推定し sqrt(N) を返す.

    取引が時間軸で不均一なため, 「実観測期間中の取引数」から年率換算する.
    """
    if trades_df.height < 2:
        return math.sqrt(max(fallback_n_per_year, 1))
    span = trades_df["entry_ts"].max() - trades_df["entry_ts"].min()
    if not isinstance(span, timedelta) or span.total_seconds() <= 0:
        return math.sqrt(max(fallback_n_per_year, 1))
    seconds_per_year = 365.25 * 24 * 3600
    n_per_year = trades_df.height * (seconds_per_year / span.total_seconds())
    return math.sqrt(max(n_per_year, 1.0))


def compute_perf_stats(
    trades: pl.DataFrame,
    fallback_n_per_year: int = 252,
) -> PerfStats:
    """trades DataFrame (persistence 形式) から PerfStats を計算.

    入力: persistence.trades_to_rows() が出すカラム
        net_pnl_usd / cost_usd / net_bps / size_usd / entry_ts ...
    """
    if trades is None or trades.is_empty():
        return PerfStats.empty()

    df = trades.sort("entry_ts")
    n = df.height
    total_pnl = float(df["net_pnl_usd"].sum() or 0.0)
    total_cost = float(df["cost_usd"].sum() or 0.0)

    bps = df["net_bps"].drop_nulls()
    if bps.is_empty():
        return PerfStats.empty()
    mean_bps = float(bps.mean() or 0.0)
    std_bps = float(bps.std() or 0.0)

    ann = _annualization_factor(df, fallback_n_per_year)
    sharpe = (mean_bps / std_bps) * ann if std_bps > 0 else 0.0
    # Sharpe SE 大標本近似: sqrt((1 + sharpe^2/2) / n)
    se = math.sqrt((1 + (sharpe**2) / 2.0) / max(n, 1))
    ci_low = sharpe - 1.96 * se
    ci_high = sharpe + 1.96 * se

    downside = bps.filter(bps < 0)
    if not downside.is_empty():
        d_std = float(downside.std() or 0.0)
        sortino = (mean_bps / d_std) * ann if d_std > 0 else 0.0
    else:
        sortino = float("inf") if mean_bps > 0 else 0.0

    # equity curve から max drawdown
    cum = df.with_columns(pl.col("net_pnl_usd").cum_sum().alias("cum"))
    cum_vals = cum["cum"].to_list()
    peak = -math.inf
    max_dd_usd = 0.0
    peak_for_dd = 0.0
    for v in cum_vals:
        if v > peak:
            peak = v
        dd = v - peak  # 負値
        if dd < max_dd_usd:
            max_dd_usd = dd
            peak_for_dd = peak
    # max DD を peak からの割合へ. 初期 0 から見て最初に peak が立つまで peak=0 → 割合計算回避
    if peak_for_dd > 0:
        max_dd_pct = max_dd_usd / peak_for_dd
    else:
        # サイズに対する比率で代用 (mean size を分母に)
        mean_size = float(df["size_usd"].abs().mean() or 1.0)
        max_dd_pct = max_dd_usd / mean_size if mean_size > 0 else 0.0

    calmar = (mean_bps / 10000.0 * ann**2) / abs(max_dd_pct) if max_dd_pct < 0 else 0.0

    wins = df.filter(pl.col("net_pnl_usd") > 0).height
    hit = wins / n if n > 0 else 0.0

    sum_wins = float(df.filter(pl.col("net_pnl_usd") > 0)["net_pnl_usd"].sum() or 0.0)
    sum_losses = float(df.filter(pl.col("net_pnl_usd") < 0)["net_pnl_usd"].sum() or 0.0)
    pf = sum_wins / abs(sum_losses) if sum_losses < 0 else float("inf") if sum_wins > 0 else 0.0

    return PerfStats(
        n_trades=n,
        total_pnl_usd=total_pnl,
        total_cost_usd=total_cost,
        sharpe_annualized=sharpe,
        sharpe_se=se,
        sharpe_ci_low=ci_low,
        sharpe_ci_high=ci_high,
        sortino_annualized=sortino,
        calmar=calmar,
        max_drawdown_pct=max_dd_pct,
        hit_rate=hit,
        profit_factor=pf,
        expectancy_bps=mean_bps,
        mean_bps=mean_bps,
        std_bps=std_bps,
    )


def equity_curve(trades: pl.DataFrame) -> pl.DataFrame:
    """累積 P/L 時系列. columns: ts, cum_pnl_usd, cum_pnl_bps_per_size."""
    if trades is None or trades.is_empty():
        return pl.DataFrame(
            schema={
                "ts": pl.Datetime,
                "cum_pnl_usd": pl.Float64,
                "cum_pnl_bps": pl.Float64,
            }
        )
    df = trades.sort("entry_ts")
    return df.with_columns(
        [
            pl.col("net_pnl_usd").cum_sum().alias("cum_pnl_usd"),
            pl.col("net_bps").cum_sum().alias("cum_pnl_bps"),
        ]
    ).select(
        [
            pl.col("entry_ts").alias("ts"),
            "cum_pnl_usd",
            "cum_pnl_bps",
        ]
    )


def underwater_curve(trades: pl.DataFrame) -> pl.DataFrame:
    """drawdown 時系列. columns: ts, drawdown_usd, drawdown_pct.

    drawdown_pct は実現 peak (>0) 比. peak が 0 以下の場合は size に対する比率.
    """
    if trades is None or trades.is_empty():
        return pl.DataFrame(
            schema={
                "ts": pl.Datetime,
                "drawdown_usd": pl.Float64,
                "drawdown_pct": pl.Float64,
            }
        )
    df = trades.sort("entry_ts").with_columns(pl.col("net_pnl_usd").cum_sum().alias("cum"))
    cum_vals = df["cum"].to_list()
    ts_vals = df["entry_ts"].to_list()
    sizes = df["size_usd"].to_list()
    out_ts: list[datetime] = []
    out_dd_usd: list[float] = []
    out_dd_pct: list[float] = []
    peak = 0.0
    for v, ts, sz in zip(cum_vals, ts_vals, sizes, strict=True):
        if v > peak:
            peak = v
        dd_usd = v - peak  # 負値
        if peak > 0:
            dd_pct = dd_usd / peak
        else:
            base = abs(sz) if sz else 1.0
            dd_pct = dd_usd / base
        out_ts.append(ts)
        out_dd_usd.append(dd_usd)
        out_dd_pct.append(dd_pct)
    return pl.DataFrame(
        {
            "ts": out_ts,
            "drawdown_usd": out_dd_usd,
            "drawdown_pct": out_dd_pct,
        }
    )


def regime_breakdown(trades: pl.DataFrame) -> pl.DataFrame:
    """regime (= trades に regime カラムがある場合のみ) 別 PerfStats サマリ.

    Phase 1.5 では trades persistence に regime カラムが無いので, この関数は
    将来 (FilledTrade.metadata['regime'] を埋めて persist 拡張する PR) で使う.
    現状は呼ばれた時に空 DF を返す.
    """
    if trades is None or trades.is_empty() or "regime" not in trades.columns:
        return pl.DataFrame(
            schema={
                "regime": pl.Utf8,
                "n": pl.Int64,
                "sharpe": pl.Float64,
                "hit_rate": pl.Float64,
                "expectancy_bps": pl.Float64,
            }
        )
    rows: list[dict] = []
    for regime in trades["regime"].unique().drop_nulls().to_list():
        sub = trades.filter(pl.col("regime") == regime)
        ps = compute_perf_stats(sub)
        rows.append(
            {
                "regime": regime,
                "n": ps.n_trades,
                "sharpe": ps.sharpe_annualized,
                "hit_rate": ps.hit_rate,
                "expectancy_bps": ps.expectancy_bps,
            }
        )
    return pl.DataFrame(rows)
