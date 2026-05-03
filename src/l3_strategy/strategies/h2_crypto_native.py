"""戦略 H2: Crypto Native 相関 (Issue #36).

仮説 (v3 §2.2): closure 中, HL TradFi perp は BTC/ETH の動きに引きずられる.
Phase 1 K8 で過去相関を分析した結果を踏まえ, 高相関期間で BTC 急変動と同方向にベット.

エントリー条件:
- regime in (R2, R3, R4) かつ regime_uncertain=False
- 直近 N bar の BTC リターン累積が ±閾値超
- 同方向 (long/short) を TradFi perp で取る

エグジット:
- holding timeout (BacktestEngine の exit_after_minutes)
- IPD 反転 (drift が逆向きに進んだら早期 exit) — Phase 2 で追加

依存: MarketState.extras に BTC ベンチマークの crypto_ret_cum を入れる
(Phase 2 で L2 pipeline 拡張要. 暫定: state.extras.get('btc_ret_cum'))
"""

from __future__ import annotations

from dataclasses import dataclass, field

from src.l3_strategy.interface import MarketState, Side, Signal, Strategy

CLOSURE_REGIMES = {
    "R2_closure_weekend",
    "R3_closure_daily",
    "R4_closure_holiday",
}

EXTRA_BTC_RET_KEY = "btc_ret_cum"  # MarketState.extras から取る key


@dataclass
class H2CryptoNative(Strategy):
    name: str = "H2_crypto_native"
    btc_ret_threshold: float = 0.005  # 0.5% in window
    base_size_usd: float = 1000.0
    expected_pnl_bps: float = 10.0
    target_symbols: tuple[str, ...] = ("xyz:SP500", "xyz:XYZ100")
    _signal_buffer: dict = field(default_factory=dict)

    def on_bar(self, state: MarketState) -> Signal | None:
        if state.regime not in CLOSURE_REGIMES:
            return None
        if state.symbol not in self.target_symbols:
            return None
        btc_ret_cum = state.extras.get(EXTRA_BTC_RET_KEY)
        if btc_ret_cum is None:
            return None

        side: Side = "flat"
        if btc_ret_cum > self.btc_ret_threshold:
            side = "long"
        elif btc_ret_cum < -self.btc_ret_threshold:
            side = "short"
        else:
            return None

        return Signal(
            timestamp=state.timestamp,
            symbol=state.symbol,
            side=side,
            size_usd=self.base_size_usd,
            expected_pnl_bps=self.expected_pnl_bps,
            confidence=min(abs(btc_ret_cum) / (self.btc_ret_threshold * 2), 1.0),
            metadata={
                "btc_ret_cum": btc_ret_cum,
                "regime": state.regime,
            },
        )
