"""戦略 H2 / H3 の動作テスト (合成データ)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from src.l3_strategy.interface import MarketState, Signal
from src.l3_strategy.strategies.h2_crypto_native import (
    EXTRA_BTC_RET_KEY,
    H2CryptoNative,
)
from src.l3_strategy.strategies.h3_cme_maintenance import H3CmeMaintenance


def _state(
    *,
    ts: datetime,
    regime: str = "R2_closure_weekend",
    symbol: str = "xyz:SP500",
    ipd: float = 0.0,
    btc_ret_cum: float | None = None,
) -> MarketState:
    extras: dict = {}
    if btc_ret_cum is not None:
        extras[EXTRA_BTC_RET_KEY] = btc_ret_cum
    return MarketState(
        timestamp=ts,
        symbol=symbol,
        mid=100.0,
        funding_rate=0.0001,
        impact_bid=99.95,
        impact_ask=100.05,
        ipd=ipd,
        regime=regime,
        regime_uncertain=False,
        extras=extras,
    )


def test_h2_long_when_btc_ret_high() -> None:
    strat = H2CryptoNative(btc_ret_threshold=0.005)
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    sig = strat.on_bar(_state(ts=base, btc_ret_cum=0.01))
    assert sig is not None
    assert sig.side == "long"


def test_h2_short_when_btc_ret_low() -> None:
    strat = H2CryptoNative(btc_ret_threshold=0.005)
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    sig = strat.on_bar(_state(ts=base, btc_ret_cum=-0.02))
    assert sig is not None
    assert sig.side == "short"


def test_h2_skips_active_regime() -> None:
    strat = H2CryptoNative(btc_ret_threshold=0.005)
    base = datetime(2026, 5, 5, 12, tzinfo=UTC)
    sig = strat.on_bar(_state(ts=base, regime="R1_active", btc_ret_cum=0.05))
    assert sig is None


def test_h2_skips_when_no_btc_ret() -> None:
    strat = H2CryptoNative()
    sig = strat.on_bar(_state(ts=datetime(2026, 5, 9, 12, tzinfo=UTC)))
    assert sig is None


def test_h3_short_when_cum_ipd_positive_in_r3() -> None:
    strat = H3CmeMaintenance(window=10, cum_ipd_entry_threshold=3.0)
    base = datetime(2026, 5, 6, 22, tzinfo=UTC)  # ET 17-18 域
    sig: Signal | None = None
    for i in range(10):
        sig = strat.on_bar(
            _state(
                ts=base + timedelta(seconds=i),
                regime="R3_closure_daily",
                ipd=1.0,
            )
        )
    assert sig is not None
    assert sig.side == "short"


def test_h3_skips_outside_r3() -> None:
    strat = H3CmeMaintenance(window=5, cum_ipd_entry_threshold=2.0)
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    for i in range(10):
        sig = strat.on_bar(
            _state(
                ts=base + timedelta(seconds=i),
                regime="R2_closure_weekend",
                ipd=1.0,
            )
        )
    assert sig is None  # H3 は R3 のみ
