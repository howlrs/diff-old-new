"""BacktestResult の Parquet 永続化テスト (PR-A)."""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pyarrow.parquet as pq

from src.config import StorageConfig
from src.l3_strategy.backtest import BacktestResult
from src.l3_strategy.interface import FilledTrade
from src.l3_strategy.persistence import save_backtest_result


def _trade(symbol: str = "xyz:SP500", net: float = 5.0) -> FilledTrade:
    return FilledTrade(
        entry_ts=datetime(2026, 5, 9, 12, 0, tzinfo=UTC),
        exit_ts=datetime(2026, 5, 9, 12, 30, tzinfo=UTC),
        symbol=symbol,
        side="long",
        size_usd=1000.0,
        entry_px=100.0,
        exit_px=100.5,
        gross_pnl_usd=5.0 + net,  # placeholder
        cost_usd=net * 0.5,
        net_pnl_usd=net,
        holding_minutes=30.0,
    )


def test_save_creates_parquet(tmp_path: Path) -> None:
    cfg = StorageConfig(curated_data_root=tmp_path / "curated")
    result = BacktestResult(
        strategy_name="H1_test",
        n_trades=2,
        gross_pnl_usd=20.0,
        cost_usd=2.0,
        net_pnl_usd=18.0,
        mean_net_bps=10.0,
        std_net_bps=2.0,
        se_bps=1.0,
        win_rate=1.0,
        trades=[_trade(net=5.0), _trade(net=3.0)],
    )
    out = save_backtest_result(
        result,
        cfg,
        symbol_filter="xyz:SP500",
        exit_after_minutes=60,
        taker_fee_rate=0.00045,
        funding_multiplier=0.5,
    )
    assert out is not None and out.exists()

    table = pq.read_table(out)
    assert table.num_rows == 2
    cols = set(table.column_names)
    expected = {
        "run_id",
        "strategy_name",
        "symbol",
        "side",
        "entry_ts",
        "exit_ts",
        "net_pnl_usd",
        "net_bps",
        "gross_bps",
        "run_started_at",
        "run_taker_fee_rate",
        "run_funding_multiplier",
    }
    assert expected.issubset(cols)


def test_save_empty_returns_none(tmp_path: Path) -> None:
    cfg = StorageConfig(curated_data_root=tmp_path / "curated")
    result = BacktestResult(
        strategy_name="H1_empty",
        n_trades=0,
        gross_pnl_usd=0.0,
        cost_usd=0.0,
        net_pnl_usd=0.0,
        mean_net_bps=0.0,
        std_net_bps=0.0,
        se_bps=0.0,
        win_rate=0.0,
        trades=[],
    )
    out = save_backtest_result(
        result,
        cfg,
        symbol_filter=None,
        exit_after_minutes=60,
        taker_fee_rate=0.00045,
        funding_multiplier=0.5,
    )
    assert out is None
