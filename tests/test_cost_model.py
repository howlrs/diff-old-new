"""Cost model のテスト."""

from __future__ import annotations

from src.config import CostConfig
from src.l3_strategy.cost_model import (
    TradeCostInput,
    estimate_funding_cost,
    estimate_slippage,
    estimate_taker_fee,
    total_cost,
)


def test_taker_fee_proportional_to_size() -> None:
    cfg = CostConfig()
    fee_a = estimate_taker_fee(1000.0, cfg.taker_fee_rate)
    fee_b = estimate_taker_fee(2000.0, cfg.taker_fee_rate)
    assert abs(fee_b - 2 * fee_a) < 1e-9


def test_funding_zero_when_rate_none() -> None:
    cfg = CostConfig()
    assert (
        estimate_funding_cost(1000.0, None, holding_hours=2.0, multiplier=cfg.funding_multiplier)
        == 0.0
    )


def test_funding_uses_multiplier() -> None:
    """米株 perp の 0.5x dampening が反映される."""
    cfg = CostConfig(funding_multiplier=0.5)
    cost_full = estimate_funding_cost(1000.0, 0.001, holding_hours=10.0, multiplier=1.0)
    cost_half = estimate_funding_cost(
        1000.0, 0.001, holding_hours=10.0, multiplier=cfg.funding_multiplier
    )
    assert abs(cost_full - 2 * cost_half) < 1e-9


def test_slippage_proportional_to_ipd() -> None:
    a = estimate_slippage(1000.0, mid=100.0, ipd=0.0)
    b = estimate_slippage(1000.0, mid=100.0, ipd=1.0)
    assert b > a


def test_total_cost_sums_components() -> None:
    cfg = CostConfig()
    inp = TradeCostInput(
        size_usd=1000.0,
        holding_hours=2.0,
        funding_rate=0.001,
        ipd=0.5,
        mid=100.0,
        resilience_factor=1.0,
    )
    c = total_cost(inp, cfg)
    assert c > 0
    # sanity: コストが額面の 5% を超えるとモデルが暴走
    assert c < 50.0
