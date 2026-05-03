"""altair チャート関数集.

GUI dashboard が呼ぶ. 戻り値は altair.Chart で marimo の mo.ui.altair_chart() に渡す.
"""

from __future__ import annotations

import altair as alt
import polars as pl


def equity_curve_chart(
    df: pl.DataFrame,
    *,
    width: int = 700,
    height: int = 240,
) -> alt.Chart:
    """累積 P/L (USD) 時系列."""
    if df.is_empty():
        df = pl.DataFrame({"ts": [], "cum_pnl_usd": []})
    return (
        alt.Chart(df.to_pandas())
        .mark_line(color="#1f77b4")
        .encode(
            x=alt.X("ts:T", title="time"),
            y=alt.Y("cum_pnl_usd:Q", title="cumulative net P/L (USD)"),
            tooltip=["ts:T", "cum_pnl_usd:Q"],
        )
        .properties(width=width, height=height, title="Equity curve")
        .interactive()
    )


def underwater_chart(
    df: pl.DataFrame,
    *,
    width: int = 700,
    height: int = 180,
) -> alt.Chart:
    """drawdown 時系列 (負値の area chart)."""
    if df.is_empty():
        df = pl.DataFrame({"ts": [], "drawdown_pct": []})
    return (
        alt.Chart(df.to_pandas())
        .mark_area(color="#d62728", opacity=0.5)
        .encode(
            x=alt.X("ts:T", title="time"),
            y=alt.Y(
                "drawdown_pct:Q",
                title="drawdown (fraction)",
                axis=alt.Axis(format=".1%"),
            ),
            tooltip=["ts:T", alt.Tooltip("drawdown_pct:Q", format=".2%")],
        )
        .properties(width=width, height=height, title="Underwater curve")
        .interactive()
    )


def trade_pnl_histogram(
    trades: pl.DataFrame,
    *,
    width: int = 500,
    height: int = 200,
    column: str = "net_bps",
) -> alt.Chart:
    """trade 単位 P/L (bps) のヒストグラム."""
    if trades.is_empty() or column not in trades.columns:
        return alt.Chart(pl.DataFrame({column: []}).to_pandas()).mark_bar()
    return (
        alt.Chart(trades.select(column).to_pandas())
        .mark_bar()
        .encode(
            x=alt.X(f"{column}:Q", bin=alt.Bin(maxbins=40), title=column),
            y=alt.Y("count():Q", title="count"),
            tooltip=[alt.Tooltip("count():Q", title="trades")],
        )
        .properties(width=width, height=height, title=f"{column} distribution")
    )


def ipd_time_series_chart(
    features: pl.DataFrame,
    symbol: str,
    *,
    width: int = 700,
    height: int = 200,
) -> alt.Chart:
    """L2 features の IPD 時系列を 1 銘柄でプロット (regime 色分け)."""
    if features.is_empty() or "ipd" not in features.columns:
        return alt.Chart(pl.DataFrame({"exchange_ts": [], "ipd": []}).to_pandas())
    sub = features.filter(pl.col("symbol") == symbol).select(["exchange_ts", "ipd", "regime"])
    return (
        alt.Chart(sub.to_pandas())
        .mark_line(opacity=0.7)
        .encode(
            x=alt.X("exchange_ts:T", title="time"),
            y=alt.Y("ipd:Q", title="IPD"),
            color=alt.Color("regime:N", title="regime"),
            tooltip=["exchange_ts:T", "ipd:Q", "regime:N"],
        )
        .properties(width=width, height=height, title=f"IPD time series — {symbol}")
        .interactive()
    )


def book_top_depth_chart(
    l2book: pl.DataFrame,
    symbol: str,
    *,
    width: int = 700,
    height: int = 200,
) -> alt.Chart:
    """top-of-book best_bid / best_ask の時系列."""
    if l2book.is_empty():
        return alt.Chart(pl.DataFrame({"exchange_ts": [], "best_bid": []}).to_pandas())
    sub = (
        l2book.filter(pl.col("symbol") == symbol)
        .select(["exchange_ts", "best_bid", "best_ask"])
        .melt(id_vars="exchange_ts", value_name="px", variable_name="side")
    )
    return (
        alt.Chart(sub.to_pandas())
        .mark_line()
        .encode(
            x=alt.X("exchange_ts:T", title="time"),
            y=alt.Y("px:Q", title="price"),
            color="side:N",
            tooltip=["exchange_ts:T", "px:Q", "side:N"],
        )
        .properties(width=width, height=height, title=f"top-of-book — {symbol}")
        .interactive()
    )
