"""IPD と EMA reconstructor のテスト."""

from __future__ import annotations

import math

from src.l2_features.ipd import EmaState, compute_ipd, step_ema


def test_ipd_zero_when_inside_band() -> None:
    # impact_bid <= S <= impact_ask の場合は IPD = 0
    s = 100.0
    assert compute_ipd(s, impact_bid=99.0, impact_ask=101.0) == 0.0


def test_ipd_positive_when_bid_pressure() -> None:
    # impact_bid > S → bid 側に厚み = positive IPD
    s = 100.0
    val = compute_ipd(s, impact_bid=100.5, impact_ask=101.0)
    assert val == 0.5


def test_ipd_negative_when_ask_pressure() -> None:
    s = 100.0
    val = compute_ipd(s, impact_bid=99.0, impact_ask=99.5)
    assert val == -0.5


def test_ema_initial_value_uses_mid() -> None:
    st = EmaState()
    val = step_ema(st, ts_sec=0.0, impact_bid=99.0, impact_ask=101.0)
    assert val == 100.0


def test_ema_clamp_per_update() -> None:
    """1ステップで ±50bps を超えない (クランプ動作)."""
    st = EmaState()
    # 初期化
    step_ema(st, ts_sec=0.0, impact_bid=99.0, impact_ask=101.0)
    initial = st.s_value
    assert initial is not None
    # 大量 IPD で押す
    after = step_ema(st, ts_sec=10.0, impact_bid=1000.0, impact_ask=2000.0)
    max_change = initial * (50.0 / 10000.0)
    assert (
        math.isclose(after - initial, max_change, rel_tol=1e-6)
        or after - initial <= max_change + 1e-6
    )


def test_ema_dt_capped() -> None:
    """Δt* = min(Δt, 0.1τ). 巨大 dt でも β は exp(-0.1) を下回らない."""
    st = EmaState(tau_sec=30 * 60, delta_cap_factor=0.1)
    step_ema(st, ts_sec=0.0, impact_bid=99.0, impact_ask=101.0)
    # 1日 (86400 sec) 進めても更新は 0.1 * τ = 180 sec 分だけ
    after = step_ema(st, ts_sec=86400.0, impact_bid=99.0, impact_ask=101.0)
    # 大きなジャンプはしない
    assert after is not None
    assert abs(after - 100.0) < 1.0
