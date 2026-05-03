"""Spread / pair calculator.

SP500 vs XYZ100, BTC ratio 等のローリング指標.
Issue #11 拡充: Engle-Granger cointegration test と OU half-life 推定.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import polars as pl
from statsmodels.regression.linear_model import OLS
from statsmodels.tools.tools import add_constant
from statsmodels.tsa.stattools import adfuller


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


@dataclass
class CointegrationResult:
    """Engle-Granger cointegration test の結果."""

    beta: float  # spread = a - beta*b の係数
    intercept: float
    spread_mean: float
    spread_std: float
    adf_stat: float
    adf_pvalue: float
    is_cointegrated: bool  # adf_pvalue < 0.05
    half_life_bars: float | None  # OU half-life (None = mean reversion 弱)
    n: int


def engle_granger_cointegration(
    a: list[float] | np.ndarray,
    b: list[float] | np.ndarray,
    significance: float = 0.05,
) -> CointegrationResult:
    """Engle-Granger 2-step cointegration:
    1. OLS  a_t = alpha + beta·b_t + eps_t
    2. residual eps_t に ADF テスト
    3. 残差の半減期 (OU process) を計算
    """
    a_arr = np.asarray(a, dtype=float)
    b_arr = np.asarray(b, dtype=float)
    mask = np.isfinite(a_arr) & np.isfinite(b_arr)
    a_arr = a_arr[mask]
    b_arr = b_arr[mask]
    n = len(a_arr)
    if n < 30:
        return CointegrationResult(
            beta=float("nan"),
            intercept=float("nan"),
            spread_mean=float("nan"),
            spread_std=float("nan"),
            adf_stat=float("nan"),
            adf_pvalue=1.0,
            is_cointegrated=False,
            half_life_bars=None,
            n=n,
        )

    X = add_constant(b_arr)  # noqa: N806  (statistics convention)
    res = OLS(a_arr, X).fit()
    intercept = float(res.params[0])
    beta = float(res.params[1])
    residual = a_arr - intercept - beta * b_arr
    spread_mean = float(np.mean(residual))
    spread_std = float(np.std(residual))

    adf = adfuller(residual, autolag="AIC")
    adf_stat = float(adf[0])
    pvalue = float(adf[1])
    is_cointeg = pvalue < significance

    # OU half-life: Deltaresidual = theta (mu - residual) Deltat + sigma dW
    # OLS 回帰: Deltar = a + b*r_{t-1} → b = -theta → half-life = ln(2)/theta
    diff = np.diff(residual)
    lag = residual[:-1]
    if len(diff) >= 10 and np.std(lag) > 0:
        Xh = add_constant(lag)  # noqa: N806
        ou_res = OLS(diff, Xh).fit()
        theta = -float(ou_res.params[1])
        half_life: float | None = float(np.log(2) / theta) if theta > 0 else None
    else:
        half_life = None

    return CointegrationResult(
        beta=beta,
        intercept=intercept,
        spread_mean=spread_mean,
        spread_std=spread_std,
        adf_stat=adf_stat,
        adf_pvalue=pvalue,
        is_cointegrated=is_cointeg,
        half_life_bars=half_life,
        n=n,
    )
