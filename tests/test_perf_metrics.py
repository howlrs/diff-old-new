"""perf_metrics の数値テスト."""

from __future__ import annotations

import math
from datetime import UTC, datetime, timedelta

import polars as pl

from src.gui.perf_metrics import (
    PerfStats,
    compute_perf_stats,
    equity_curve,
    underwater_curve,
)


def _trade_row(*, ts: datetime, net_pnl: float, size: float = 1000.0) -> dict:
    return {
        "entry_ts": ts,
        "exit_ts": ts + timedelta(minutes=30),
        "symbol": "xyz:SP500",
        "side": "long",
        "size_usd": size,
        "net_pnl_usd": net_pnl,
        "cost_usd": 0.5,
        "net_bps": net_pnl / size * 10000.0,
        "gross_bps": (net_pnl + 0.5) / size * 10000.0,
        "gross_pnl_usd": net_pnl + 0.5,
        "entry_px": 100.0,
        "exit_px": 100.0 + net_pnl / size * 100.0,
        "holding_minutes": 30.0,
    }


def test_empty_returns_empty_stats() -> None:
    df = pl.DataFrame()
    ps = compute_perf_stats(df)
    assert ps == PerfStats.empty()


def test_all_winners_profit_factor_inf() -> None:
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    rows = [_trade_row(ts=base + timedelta(hours=i), net_pnl=10.0) for i in range(10)]
    df = pl.DataFrame(rows)
    ps = compute_perf_stats(df)
    assert ps.n_trades == 10
    assert ps.hit_rate == 1.0
    assert math.isinf(ps.profit_factor)
    assert ps.total_pnl_usd == 100.0


def test_mixed_pnl_metrics_match_handcalc() -> None:
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    pnls = [10.0, -5.0, 20.0, -8.0, 15.0]
    rows = [_trade_row(ts=base + timedelta(hours=i), net_pnl=p) for i, p in enumerate(pnls)]
    df = pl.DataFrame(rows)
    ps = compute_perf_stats(df)
    assert ps.n_trades == 5
    assert ps.total_pnl_usd == sum(pnls)
    # hit rate = 3/5
    assert abs(ps.hit_rate - 0.6) < 1e-9
    # profit factor = (10+20+15) / (5+8) = 45/13
    assert abs(ps.profit_factor - 45 / 13) < 1e-6
    # mean_bps = mean(pnls) / 1000 * 10000 = mean(pnls) * 10
    assert abs(ps.mean_bps - sum(pnls) / len(pnls) * 10) < 1e-6


def test_max_drawdown_negative() -> None:
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    pnls = [10.0, 5.0, -30.0, 5.0]  # peak=15, trough=-15 → DD=-30
    rows = [_trade_row(ts=base + timedelta(hours=i), net_pnl=p) for i, p in enumerate(pnls)]
    df = pl.DataFrame(rows)
    ps = compute_perf_stats(df)
    # max_dd_pct: peak(15) からの drawdown = -30 → -30/15 = -2.0
    assert ps.max_drawdown_pct < 0
    assert ps.max_drawdown_pct == -2.0


def test_sharpe_positive_when_mean_positive() -> None:
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    pnls = [10.0, 8.0, 12.0, 11.0, 9.0, 10.5, 11.5, 10.0]
    rows = [_trade_row(ts=base + timedelta(hours=i), net_pnl=p) for i, p in enumerate(pnls)]
    df = pl.DataFrame(rows)
    ps = compute_perf_stats(df)
    assert ps.sharpe_annualized > 0
    assert ps.sharpe_ci_low < ps.sharpe_annualized < ps.sharpe_ci_high


def test_equity_and_underwater_curves_have_n_rows() -> None:
    base = datetime(2026, 5, 9, 12, tzinfo=UTC)
    rows = [
        _trade_row(ts=base + timedelta(hours=i), net_pnl=p) for i, p in enumerate([5.0, -3.0, 10.0])
    ]
    df = pl.DataFrame(rows)

    eq = equity_curve(df)
    assert eq.height == 3
    cum = eq["cum_pnl_usd"].to_list()
    assert cum[0] == 5.0
    assert cum[1] == 2.0
    assert cum[2] == 12.0

    uw = underwater_curve(df)
    assert uw.height == 3
    # peak は 5 → -3 で trough = 2 → DD = -3, peak 12 で 0
    dd = uw["drawdown_usd"].to_list()
    assert max(dd) == 0.0  # peak の瞬間
    assert min(dd) <= -3.0
