"""Cost model: taker fee + funding (0.5x) + slippage (IPD-based).

v3 §4.3 + Gemini指摘: resilience を slippage に組み込む.
Phase 1 では resilience が無い場合は IPD のみで近似. Phase 2 で実測値に置き換え.
"""

from __future__ import annotations

from dataclasses import dataclass

from src.config import CostConfig


@dataclass
class TradeCostInput:
    """エントリー / エグジット 両時点の slippage を別々に計算するための入力.

    Gemini指摘 (Bug 3) 反映: ipd/mid は exit 時点だが,
    entry 時点の状態を別途渡せるようにする.
    """

    size_usd: float
    holding_hours: float
    funding_rate: float | None
    ipd: float | None  # exit 時点 ipd (互換性のため残す)
    mid: float  # exit 時点 mid
    resilience_factor: float = 1.0  # 1.0 = 板すぐ戻る, 大きいほど slippage 増
    entry_ipd: float | None = None  # 指定なければ ipd と同値とみなす
    entry_mid: float | None = None  # 指定なければ mid と同値とみなす


def estimate_taker_fee(size_usd: float, fee_rate: float) -> float:
    return abs(size_usd) * fee_rate


def estimate_funding_cost(
    size_usd: float,
    funding_rate: float | None,
    holding_hours: float,
    multiplier: float,
) -> float:
    """funding は 0.5x dampening (米株 perp). 1時間ごと支払い."""
    if funding_rate is None:
        return 0.0
    # holding_hours の間に支払い回数 = floor(holding_hours)
    # 簡易: rate * size * hours * multiplier
    return abs(size_usd) * abs(funding_rate) * holding_hours * multiplier


def estimate_slippage(
    size_usd: float,
    mid: float,
    ipd: float | None,
    resilience_factor: float = 1.0,
) -> float:
    """IPD ベースの slippage 推定 (Phase 1 簡易).

    IPD が大きいほど板薄 → slippage 増. resilience_factor は大口Taker後の
    板回復が遅いほど 1.0 を超える.
    """
    if ipd is None or mid <= 0:
        # default: 1 bp の slippage
        return abs(size_usd) * 0.0001
    ipd_pct = abs(ipd) / mid
    # 経験的: slippage ≈ size_usd * (ipd_pct + 1bp) * resilience_factor
    return abs(size_usd) * (ipd_pct + 0.0001) * resilience_factor


def total_cost(
    inp: TradeCostInput,
    cfg: CostConfig,
) -> float:
    """エントリー + エグジット 両方分のコスト合計.

    Gemini指摘 (Bug 3) 反映: entry / exit の slippage を別々に計算して合算.
    """
    fee = estimate_taker_fee(inp.size_usd, cfg.taker_fee_rate) * 2  # entry + exit
    funding = estimate_funding_cost(
        inp.size_usd,
        inp.funding_rate,
        inp.holding_hours,
        cfg.funding_multiplier,
    )
    entry_ipd = inp.entry_ipd if inp.entry_ipd is not None else inp.ipd
    entry_mid = inp.entry_mid if inp.entry_mid is not None else inp.mid
    slip_entry = estimate_slippage(inp.size_usd, entry_mid, entry_ipd, inp.resilience_factor)
    slip_exit = estimate_slippage(inp.size_usd, inp.mid, inp.ipd, inp.resilience_factor)
    return fee + funding + slip_entry + slip_exit
