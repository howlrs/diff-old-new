"""Spread / pair calculator.

SP500 vs XYZ100, BTC ratio 等のローリング指標.
"""

from __future__ import annotations

import polars as pl


def join_pair_ohlc(
    df_a: pl.DataFrame,
    df_b: pl.DataFrame,
    ts_col: str = "exchange_ts",
    px_col: str = "mid",
    suffix_a: str = "a",
    suffix_b: str = "b",
) -> pl.DataFrame:
    """2銘柄を timestamp で as-of 結合."""
    a = df_a.select([ts_col, px_col]).rename({px_col: f"px_{suffix_a}"})
    b = df_b.select([ts_col, px_col]).rename({px_col: f"px_{suffix_b}"})
    a = a.sort(ts_col)
    b = b.sort(ts_col)
    return a.join_asof(b, on=ts_col, strategy="backward")


def add_log_ratio(
    df: pl.DataFrame,
    a_col: str,
    b_col: str,
    out_col: str = "log_ratio",
) -> pl.DataFrame:
    return df.with_columns(((pl.col(a_col).log()) - (pl.col(b_col).log())).alias(out_col))


def rolling_zscore(
    df: pl.DataFrame,
    col: str,
    window: int,
    out_col: str = "zscore",
) -> pl.DataFrame:
    rolling_mean = pl.col(col).rolling_mean(window_size=window).alias("_m")
    rolling_std = pl.col(col).rolling_std(window_size=window).alias("_s")
    return (
        df.with_columns([rolling_mean, rolling_std])
        .with_columns(((pl.col(col) - pl.col("_m")) / pl.col("_s")).alias(out_col))
        .drop(["_m", "_s"])
    )


def rolling_corr(
    df: pl.DataFrame,
    a_col: str,
    b_col: str,
    window: int,
    out_col: str = "corr",
) -> pl.DataFrame:
    return df.with_columns(
        pl.rolling_corr(pl.col(a_col), pl.col(b_col), window_size=window).alias(out_col)
    )
