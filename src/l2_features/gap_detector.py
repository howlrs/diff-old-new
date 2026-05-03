"""Regime境界における price gap 検出.

closure 終了 (active session 開始) 直前の HL 内部価格 と
active 開始直後の oracle/CME 直結価格 の差を集計.
"""

from __future__ import annotations

import polars as pl

from src.l2_features.regime import Regime


def detect_regime_transitions(
    df: pl.DataFrame,
    regime_col: str = "regime",
) -> pl.DataFrame:
    """regime が変化する行を検出 (transition フラグ付与)."""
    df = df.sort("exchange_ts")
    return df.with_columns(
        (pl.col(regime_col) != pl.col(regime_col).shift(1))
        .fill_null(False)
        .alias("regime_transition")
    )


def compute_open_gap(
    df: pl.DataFrame,
    px_col: str = "mid",
    pre_window_min: int = 5,
    post_window_min: int = 5,
) -> pl.DataFrame:
    """各 active session 開始時の gap を計算 (Phase 1 簡易版).

    Phase 1 では transition の前後 N 本平均で gap = (post_mean - pre_mean) / pre_mean.
    Phase 2 で時系列重み付き平均、IQR ベース外れ値除去等を追加予定.
    """
    df = detect_regime_transitions(df)
    transitions = df.filter(
        (pl.col("regime_transition")) & (pl.col("regime") == Regime.ACTIVE.value)
    )
    out_rows: list[dict] = []
    for ts in transitions["exchange_ts"]:
        pre = (
            df.filter(
                (pl.col("exchange_ts") < ts)
                & (pl.col("exchange_ts") >= ts - pl.duration(minutes=pre_window_min))
            )[px_col]
            .drop_nulls()
            .mean()
        )
        post = (
            df.filter(
                (pl.col("exchange_ts") >= ts)
                & (pl.col("exchange_ts") < ts + pl.duration(minutes=post_window_min))
            )[px_col]
            .drop_nulls()
            .mean()
        )
        if pre is None or post is None or pre == 0:
            continue
        out_rows.append(
            {
                "transition_ts": ts,
                "pre_mean": pre,
                "post_mean": post,
                "gap_bps": (post - pre) / pre * 10000.0,
            }
        )
    return (
        pl.DataFrame(out_rows)
        if out_rows
        else pl.DataFrame(
            schema={
                "transition_ts": pl.Datetime,
                "pre_mean": pl.Float64,
                "post_mean": pl.Float64,
                "gap_bps": pl.Float64,
            }
        )
    )
