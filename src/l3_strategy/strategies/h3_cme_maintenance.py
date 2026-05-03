"""戦略 H3: CMEメンテ mini-closure 戻り取り (Issue #37).

仮説 (v3 §2.3): 毎日 17-18 ET の CMEメンテ時間中, HL内部 EMA が IPD 累積で
ドリフトする. メンテ終了 (18:00 ET) で oracle が CME EMM6 にワープ復帰
するため, ドリフト方向と反対にエントリーすれば mean reversion で取れる.

エントリー条件:
- regime == R3 (CLOSURE_DAILY)
- IPD 累積が ±閾値超
- regime_uncertain=False (境界 buffer 除外)

エグジット:
- regime が R3 から R1 (active) に切り替わる瞬間 = oracle ワープ
- BacktestEngine の exit_after_minutes でも fallback

H1 (week 末) と H3 (CMEメンテ) の論理は対称. H1 と組合せて年間サンプル N>=500 達成を狙う.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field

from src.l3_strategy.interface import MarketState, Side, Signal, Strategy


@dataclass
class H3CmeMaintenance(Strategy):
    name: str = "H3_cme_maintenance"
    window: int = 20
    cum_ipd_entry_threshold: float = 3.0
    base_size_usd: float = 1000.0
    expected_pnl_bps: float = 12.0
    _hist: dict[str, deque[float]] = field(default_factory=dict)

    def on_bar(self, state: MarketState) -> Signal | None:
        # CMEメンテ regime のみ
        if state.regime != "R3_closure_daily":
            self._hist.pop(state.symbol, None)
            return None
        if state.regime_uncertain:
            return None
        if state.ipd is None:
            return None

        buf = self._hist.setdefault(state.symbol, deque(maxlen=self.window))
        buf.append(state.ipd)
        if len(buf) < self.window:
            return None

        cum = sum(buf)
        side: Side = "flat"
        # ドリフト方向と反対 (mean reversion)
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
                "regime": "R3_closure_daily",
            },
        )

    def warmup_bars(self) -> int:
        return self.window
