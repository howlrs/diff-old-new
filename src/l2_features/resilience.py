"""Resilience metric: 大口Taker後の板回復時間 (Issue #13).

定義 (Phase 1 簡易版):
- 大口 Taker = trade_size_usd が直近 N trades の中央値の K倍超
- "板の回復" = top of book (best_bid/best_ask) の spread が大口前の水準±X% 以内に戻る
- 回復時間 = 大口イベント時刻から復帰時刻までの秒数

K9 KPI の素材.
"""

from __future__ import annotations

from dataclasses import dataclass

import polars as pl

DEFAULT_LARGE_TRADE_K = 10.0  # 中央値の 10 倍超で「大口」
DEFAULT_SPREAD_TOLERANCE = 1.5  # 大口前 spread の 1.5 倍以内に戻ったら「回復」
DEFAULT_RECOVERY_TIMEOUT_SEC = 300.0


@dataclass
class ResilienceEvent:
    """1 つの大口 Taker → 板回復までの記録."""

    symbol: str
    trade_ts: pl.Datetime | None
    trade_size_usd: float
    trade_side: str
    pre_spread: float
    post_spread_max: float
    recovery_sec: float | None  # None → timeout 内に回復しなかった
    is_recovered: bool


def detect_large_taker_events(
    trades: pl.DataFrame,
    *,
    window: int = 50,
    k: float = DEFAULT_LARGE_TRADE_K,
) -> pl.DataFrame:
    """trades から大口イベント行を抽出.

    Args:
        trades: columns [symbol, exchange_ts, px, sz, side, ...].
        window: ローリング中央値の窓 (trades 単位).
        k: 中央値の何倍超を「大口」とするか.

    Returns:
        DataFrame: 大口イベント行 + size_usd / median_size_usd / multiple.
    """
    if trades.is_empty():
        return trades

    df = trades.sort(["symbol", "exchange_ts"]).with_columns(
        (pl.col("px") * pl.col("sz")).alias("size_usd"),
    )
    df = df.with_columns(
        pl.col("size_usd")
        .rolling_median(window_size=window, min_samples=10)
        .over("symbol")
        .alias("median_size_usd")
    )
    df = df.with_columns((pl.col("size_usd") / pl.col("median_size_usd")).alias("size_multiple"))
    return df.filter(pl.col("size_multiple") >= k)


def compute_resilience_for_event(
    l2book: pl.DataFrame,
    event_ts,
    symbol: str,
    *,
    pre_window_sec: int = 30,
    spread_tolerance: float = DEFAULT_SPREAD_TOLERANCE,
    timeout_sec: float = DEFAULT_RECOVERY_TIMEOUT_SEC,
) -> tuple[float, float, float | None]:
    """単一イベントの (pre_spread, post_spread_max, recovery_sec).

    pre_spread: 大口直前の spread 中央値.
    post_spread_max: 大口直後の spread 最大値.
    recovery_sec: spread が pre_spread x tolerance 以内に戻った時刻までの秒数.
                  timeout 内に戻らなければ None.
    """
    sub = l2book.filter(pl.col("symbol") == symbol).sort("exchange_ts")
    if sub.is_empty():
        return (0.0, 0.0, None)

    pre = sub.filter(
        (pl.col("exchange_ts") < event_ts)
        & (pl.col("exchange_ts") >= event_ts - pl.duration(seconds=pre_window_sec))
    )
    post = sub.filter(
        (pl.col("exchange_ts") >= event_ts)
        & (pl.col("exchange_ts") < event_ts + pl.duration(seconds=int(timeout_sec)))
    )
    if pre.is_empty() or post.is_empty():
        return (0.0, 0.0, None)

    pre = pre.with_columns((pl.col("best_ask") - pl.col("best_bid")).alias("spread"))
    post = post.with_columns((pl.col("best_ask") - pl.col("best_bid")).alias("spread"))

    pre_spread = pre["spread"].drop_nulls().median()
    if pre_spread is None or pre_spread <= 0:
        return (0.0, 0.0, None)
    post_spread_max = post["spread"].drop_nulls().max() or 0.0

    threshold = pre_spread * spread_tolerance
    # 戻った瞬間
    post_with_recovered = post.with_columns((pl.col("spread") <= threshold).alias("recovered"))
    recovered_rows = post_with_recovered.filter(pl.col("recovered"))
    if recovered_rows.is_empty():
        return (float(pre_spread), float(post_spread_max), None)
    first_recovered = recovered_rows["exchange_ts"].head(1)[0]
    recovery_sec = (first_recovered - event_ts).total_seconds()
    return (float(pre_spread), float(post_spread_max), float(recovery_sec))


def compute_resilience_distribution(
    trades: pl.DataFrame,
    l2book: pl.DataFrame,
    *,
    window: int = 50,
    k: float = DEFAULT_LARGE_TRADE_K,
    pre_window_sec: int = 30,
    spread_tolerance: float = DEFAULT_SPREAD_TOLERANCE,
    timeout_sec: float = DEFAULT_RECOVERY_TIMEOUT_SEC,
) -> list[ResilienceEvent]:
    """全大口イベントの ResilienceEvent リスト."""
    events = detect_large_taker_events(trades, window=window, k=k)
    if events.is_empty():
        return []

    out: list[ResilienceEvent] = []
    for row in events.iter_rows(named=True):
        pre_spread, post_max, rec_sec = compute_resilience_for_event(
            l2book,
            row["exchange_ts"],
            row["symbol"],
            pre_window_sec=pre_window_sec,
            spread_tolerance=spread_tolerance,
            timeout_sec=timeout_sec,
        )
        out.append(
            ResilienceEvent(
                symbol=row["symbol"],
                trade_ts=row["exchange_ts"],
                trade_size_usd=float(row.get("size_usd") or 0.0),
                trade_side=row.get("side", ""),
                pre_spread=pre_spread,
                post_spread_max=post_max,
                recovery_sec=rec_sec,
                is_recovered=rec_sec is not None,
            )
        )
    return out
