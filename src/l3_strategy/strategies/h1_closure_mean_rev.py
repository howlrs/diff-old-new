"""戦略 H1: closure 中 IPD 累積ドリフト mean reversion (最小プロトタイプ).

v3 §2.1.

エントリー条件:
    - regime in (R2 weekend / R3 daily / R4 holiday)
    - regime_uncertain == false (engine が既に skip するが念のため)
    - IPD 直近 N バー累積が +閾値超 → SHORT (mean reversion 期待)
    - IPD 直近 N バー累積が -閾値超 → LONG

エグジット条件 (engine 側):
    - holding_minutes >= exit_after_minutes (デフォルト 60 分)

Phase 2 で対応:
    - regime境界 -15min での強制 exit
    - dynamic exit (IPD 累積が中立に戻ったら)
    - サイジング (IPD 大きさ・confidence ベース)
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field

from src.l3_strategy.interface import MarketState, Side, Signal, Strategy

CLOSURE_REGIMES = {
    "R2_closure_weekend",
    "R3_closure_daily",
    "R4_closure_holiday",
}


@dataclass
class H1ClosureMeanReversion(Strategy):
    name: str = "H1_closure_mean_rev"
    window: int = 30
    cum_ipd_entry_threshold: float = 5.0  # 累積 IPD で 5 unit 超ずれたらエントリー
    base_size_usd: float = 1000.0
    expected_pnl_bps: float = 15.0
    _hist: dict[str, deque[float]] = field(default_factory=dict)

    def on_bar(self, state: MarketState) -> Signal | None:
        if state.regime not in CLOSURE_REGIMES:
            self._hist.pop(state.symbol, None)
            return None
        if state.ipd is None:
            return None

        buf = self._hist.setdefault(state.symbol, deque(maxlen=self.window))
        buf.append(state.ipd)
        if len(buf) < self.window:
            return None

        cum = sum(buf)
        side: Side = "flat"
        if cum > self.cum_ipd_entry_threshold:
            side = "short"
        elif cum < -self.cum_ipd_entry_threshold:
            side = "long"
        else:
            return None

        return Signal(
            timestamp=state.timestamp,
            symbol=state.symbol,
            side=side,
            size_usd=self.base_size_usd,
            expected_pnl_bps=self.expected_pnl_bps,
            confidence=min(abs(cum) / self.cum_ipd_entry_threshold, 3.0) / 3.0,
            metadata={
                "cum_ipd": cum,
                "window": self.window,
                "regime": state.regime,
            },
        )

    def warmup_bars(self) -> int:
        return self.window
