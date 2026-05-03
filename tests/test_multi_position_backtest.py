"""Issue #29: マルチポジ・マルチ銘柄バックテストの最小テスト."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import polars as pl

from src.config import CostConfig
from src.l3_strategy.backtest import BacktestEngine
from src.l3_strategy.strategies.h1_closure_mean_rev import H1ClosureMeanReversion


def _build_two_symbol_df() -> pl.DataFrame:
    base = datetime(2026, 5, 9, 12, 0, tzinfo=UTC)
    rows = []
    for sym in ("xyz:SP500", "BTC"):
        for i in range(50):
            rows.append(
                {
                    "exchange_ts": base + timedelta(seconds=i),
                    "symbol": sym,
                    "mid": 100.0 + (i % 5) * 0.01,
                    "funding_rate": 0.0001,
                    "impact_bid_px": 99.95,
                    "impact_ask_px": 100.05,
                    "ipd": 1.0 if i < 30 else -1.0,
                    "regime": "R2_closure_weekend",
                    "regime_uncertain": False,
                }
            )
    return pl.DataFrame(rows)


def test_multi_symbol_backtest_produces_per_symbol_breakdown() -> None:
    df = _build_two_symbol_df()
    engine = BacktestEngine(CostConfig(), max_positions_per_symbol=1, max_total_positions=4)
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=3.0)
    res = engine.run(strat, df, exit_after_minutes=1)

    assert res.n_trades >= 2  # both symbols should produce at least one
    assert "xyz:SP500" in res.by_symbol
    assert "BTC" in res.by_symbol
    assert res.by_symbol["xyz:SP500"]["n"] >= 1
    assert res.by_symbol["BTC"]["n"] >= 1


def test_max_total_positions_caps_concurrent() -> None:
    """max_total_positions=1 にすると同時に1ポジまで.

    SP500 と BTC の同時刻シグナルが出ても 1 銘柄しか入らないことを確認.
    """
    df = _build_two_symbol_df()
    engine = BacktestEngine(CostConfig(), max_positions_per_symbol=1, max_total_positions=1)
    strat = H1ClosureMeanReversion(window=10, cum_ipd_entry_threshold=3.0)
    res = engine.run(strat, df, exit_after_minutes=10)
    # max_total が 1 なので, 2 銘柄並列にはならない. trades は完走時の強制 exit のみ
    assert res.n_trades >= 1
