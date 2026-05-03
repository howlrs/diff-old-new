"""LiveEngine の dry-run + Signal 生成テスト (Issue #27)."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from src.config import AppConfig
from src.l1_collector.types import AssetCtx, L2BookLevel, L2BookSnapshot
from src.l3_strategy.live import LiveEngine
from src.l3_strategy.strategies.h1_closure_mean_rev import H1ClosureMeanReversion


def test_live_engine_rejects_non_dry_run() -> None:
    cfg = AppConfig()
    strat = H1ClosureMeanReversion()
    with pytest.raises(NotImplementedError):
        LiveEngine(cfg, strat, dry_run=False)


@pytest.mark.asyncio
async def test_live_engine_handle_l2_emits_signal_when_threshold_met() -> None:
    """closure regime + 累積 IPD 高 → H1 がシグナル生成 → Live が log only.

    H1 は per-bar IPD を window 累積して entry_threshold 超えで発火.
    impact_bid > mid + 0.5 で 1 bar あたり IPD ~0.5 → 5 bar で 2.5 累積 → 閾値 2.0 で発火.
    """
    cfg = AppConfig()
    strat = H1ClosureMeanReversion(window=3, cum_ipd_entry_threshold=2.0)
    engine = LiveEngine(cfg, strat, dry_run=True)

    # ctx: impact_bid を mid (100.0) より十分上に置き IPD を強く正方向に
    ctx = AssetCtx(
        symbol="xyz:SP500",
        poll_ts=datetime(2026, 5, 9, 12, tzinfo=UTC),
        dex="xyz",
        mark_px=100.0,
        oracle_px=99.5,
        funding_rate=0.0001,
        impact_bid_px=101.0,  # mid (100) より +1.0 → IPD = +1.0 per bar
        impact_ask_px=102.0,
    )
    engine._latest_ctx["xyz:SP500"] = ctx

    # 5 バー (window=3 を満たし, 累積が閾値を超える)
    for i in range(5):
        snap = L2BookSnapshot(
            symbol="xyz:SP500",
            exchange_ts=datetime(2026, 5, 9, 12, 0, i, tzinfo=UTC),  # Saturday → R2
            recv_ts=datetime(2026, 5, 9, 12, 0, i, tzinfo=UTC),
            bids=[L2BookLevel(px=99.5, sz=1.0)],
            asks=[L2BookLevel(px=100.5, sz=1.0)],
        )
        await engine._handle_l2(snap)

    # cum_ipd = 1.0 * 3 = 3.0 > 2.0 → short signal 発火
    assert engine._signal_count.get("xyz:SP500", 0) >= 1


@pytest.mark.asyncio
async def test_live_engine_skips_when_no_ctx() -> None:
    cfg = AppConfig()
    strat = H1ClosureMeanReversion()
    engine = LiveEngine(cfg, strat, dry_run=True)

    snap = L2BookSnapshot(
        symbol="xyz:SP500",
        exchange_ts=datetime(2026, 5, 9, 12, tzinfo=UTC),
        recv_ts=datetime(2026, 5, 9, 12, tzinfo=UTC),
        bids=[L2BookLevel(px=99.5, sz=1.0)],
        asks=[L2BookLevel(px=100.5, sz=1.0)],
    )
    await engine._handle_l2(snap)
    assert engine._signal_count.get("xyz:SP500", 0) == 0
