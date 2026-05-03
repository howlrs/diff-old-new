"""H1 戦略 + backtest 統合テスト."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from src.config import CostConfig
from src.l3_strategy.backtest import BacktestEngine
from src.l3_strategy.interface import MarketState, Signal
from src.l3_strategy.strategies.h1_closure_mean_rev import (
    H1ClosureMeanReversion,
)


def _state(
    ts: datetime,
    *,
    ipd: float,
    regime: str = "R2_closure_weekend",
    mid: float = 100.0,
    funding_rate: float = 0.0001,
) -> MarketState:
    return MarketState(
        timestamp=ts,
        symbol="SP500",
        mid=mid,
        funding_rate=funding_rate,
        impact_bid=mid - 0.05,
        impact_ask=mid + 0.05,
        ipd=ipd,
        regime=regime,
        regime_uncertain=False,
    )


def test_h1_short_when_cum_ipd_positive() -> None:
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=5.0)
    base_ts = datetime(2026, 5, 9, 12, 0, tzinfo=UTC)
    sig: Signal | None = None
    for i in range(10):
        sig = strat.on_bar(_state(base_ts + timedelta(seconds=i), ipd=1.0))
    assert sig is not None
    assert sig.side == "short"


def test_h1_long_when_cum_ipd_negative() -> None:
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=5.0)
    base_ts = datetime(2026, 5, 9, 12, 0, tzinfo=UTC)
    sig: Signal | None = None
    for i in range(10):
        sig = strat.on_bar(_state(base_ts + timedelta(seconds=i), ipd=-1.0))
    assert sig is not None
    assert sig.side == "long"


def test_h1_no_signal_outside_closure() -> None:
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=5.0)
    base_ts = datetime(2026, 5, 4, 12, 0, tzinfo=UTC)
    sigs: list[Signal | None] = []
    for i in range(10):
        sigs.append(
            strat.on_bar(
                _state(
                    base_ts + timedelta(seconds=i),
                    ipd=1.0,
                    regime="R1_active",
                )
            )
        )
    assert all(s is None for s in sigs)


def test_backtest_runs_without_error() -> None:
    """合成データで H1 → BacktestEngine が完走することを確認."""
    import polars as pl

    base = datetime(2026, 5, 9, 12, 0, tzinfo=UTC)
    rows = []
    for i in range(50):
        rows.append(
            {
                "exchange_ts": base + timedelta(seconds=i),
                "symbol": "SP500",
                "mid": 100.0 + (i % 5) * 0.01,
                "funding_rate": 0.0001,
                "impact_bid_px": 99.95,
                "impact_ask_px": 100.05,
                "ipd": 1.0 if i < 30 else -1.0,
                "regime": "R2_closure_weekend",
                "regime_uncertain": False,
            }
        )
    df = pl.DataFrame(rows)
    engine = BacktestEngine(CostConfig())
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=3.0)
    result = engine.run(strat, df, symbol_filter="SP500", exit_after_minutes=1)
    assert result.strategy_name == "H1_closure_mean_rev"
    # 最低 1 取引は出るはず
    assert result.n_trades >= 1
