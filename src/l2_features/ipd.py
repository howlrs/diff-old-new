"""IPD calculator + EMA price reconstructor.

IPD = max(impact_bid - S, 0) - max(S - impact_ask, 0)
EMA: S_t = β_t * S_{t-} + (1 - β_t) * x_t,   β_t = exp(-Δt*/τ)

v3 §1.3 / §4.2 参照.
"""

from __future__ import annotations

import math
from dataclasses import dataclass


@dataclass
class EmaState:
    """連続時間 EMA の内部状態 (closure regime のみ更新)."""

    last_ts_sec: float | None = None
    s_value: float | None = None  # 現在の oracle 推定値
    tau_sec: float = 30 * 60  # τ = 30 min
    delta_cap_factor: float = 0.1  # Δt* = min(Δt, 0.1τ)
    clamp_per_update_bps: float = 50.0  # 1更新で ±50 bps 上限


def compute_ipd(
    s: float,
    impact_bid: float,
    impact_ask: float,
) -> float:
    """IPD = max(impactBid - S, 0) - max(S - impactAsk, 0)."""
    return max(impact_bid - s, 0.0) - max(s - impact_ask, 0.0)


def step_ema(
    state: EmaState,
    ts_sec: float,
    impact_bid: float,
    impact_ask: float,
) -> float:
    """closure 中の oracle EMA 1ステップ更新.

    Returns:
        次の S_t.
    """
    if state.s_value is None:
        # 初期値は現在の mid を使う (Phase 1 では暫定)
        state.s_value = (impact_bid + impact_ask) / 2.0
        state.last_ts_sec = ts_sec
        return state.s_value

    if state.last_ts_sec is None:
        state.last_ts_sec = ts_sec
        return state.s_value

    dt = max(ts_sec - state.last_ts_sec, 0.0)
    dt_capped = min(dt, state.delta_cap_factor * state.tau_sec)
    beta = math.exp(-dt_capped / state.tau_sec)

    ipd = compute_ipd(state.s_value, impact_bid, impact_ask)
    x_t = state.s_value + ipd
    next_s = beta * state.s_value + (1 - beta) * x_t

    # ±50 bps クランプ (v3 §1.3)
    max_change = state.s_value * (state.clamp_per_update_bps / 10000.0)
    next_s = max(min(next_s, state.s_value + max_change), state.s_value - max_change)

    state.s_value = next_s
    state.last_ts_sec = ts_sec
    return next_s


def cumulative_ipd_zscore(
    ipd_series: list[float],
    window: int = 50,
) -> list[float | None]:
    """累積 IPD の z-score (closure 中の anomaly 検出).

    Phase 1 プロトタイプ: シンプルな running mean / std.
    Phase 2 で EWMA + Hill 推定で改善.
    """
    cum: list[float] = []
    running_sum = 0.0
    for v in ipd_series:
        running_sum += v
        cum.append(running_sum)

    out: list[float | None] = []
    for i, c in enumerate(cum):
        if i < window:
            out.append(None)
            continue
        win = cum[i - window : i]
        mean = sum(win) / window
        var = sum((x - mean) ** 2 for x in win) / window
        std = math.sqrt(var) if var > 0 else 0.0
        out.append((c - mean) / std if std > 0 else None)
    return out
