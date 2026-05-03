"""BacktestEngine: L2 features を時系列で Strategy.on_bar に渡し,
コスト控除後 P/L と LLN/CLT 統計を集計する.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from datetime import datetime

import polars as pl

from src.config import CostConfig
from src.l3_strategy.cost_model import TradeCostInput, total_cost
from src.l3_strategy.interface import (
    FilledTrade,
    MarketState,
    Side,
    Strategy,
    df_to_market_states,
)
from src.logging_setup import get_logger

log = get_logger("l3.backtest")


@dataclass
class _OpenPosition:
    """エントリー時のマーケットスナップショット (Bug 3 修正用).

    Gemini指摘: slippage を entry/exit で別々に計算する必要があるため,
    entry 時の ipd / mid を保存しておく.
    """

    entry_ts: datetime
    side: Side
    size_usd: float
    entry_px: float
    entry_ipd: float | None
    entry_mid: float
    target_pnl_bps: float
    metadata: dict = field(default_factory=dict)


@dataclass
class BacktestResult:
    strategy_name: str
    n_trades: int
    gross_pnl_usd: float
    cost_usd: float
    net_pnl_usd: float
    mean_net_bps: float
    std_net_bps: float
    se_bps: float  # standard error = std / sqrt(N) ← CLT
    win_rate: float
    trades: list[FilledTrade]


class BacktestEngine:
    """シングルポジション・1銘柄想定の最小プロトタイプ.

    Phase 2 でマルチポジション・複数銘柄・適切な exit 規律 を実装.
    """

    def __init__(self, cost_cfg: CostConfig) -> None:
        self.cost_cfg = cost_cfg

    def run(
        self,
        strategy: Strategy,
        df: pl.DataFrame,
        *,
        symbol_filter: str | None = None,
        exit_after_minutes: int = 60,
    ) -> BacktestResult:
        if symbol_filter:
            df = df.filter(pl.col("symbol") == symbol_filter)
        df = df.sort("exchange_ts")
        states = df_to_market_states(df)
        log.info(
            "backtest.start",
            strategy=strategy.name,
            symbol=symbol_filter,
            n_bars=len(states),
        )

        trades: list[FilledTrade] = []
        open_pos: _OpenPosition | None = None

        for state in states:
            # Geminiの指摘: regime_uncertain は engine 側で skip
            if state.regime_uncertain:
                continue

            # exit の判定 (前ポジが時間切れ or 目標到達)
            if open_pos is not None:
                holding_min = (state.timestamp - open_pos.entry_ts).total_seconds() / 60
                exit_now = holding_min >= exit_after_minutes
                if exit_now:
                    trade = self._close(open_pos, state)
                    trades.append(trade)
                    open_pos = None

            if open_pos is not None:
                # まだ持ってる
                continue

            sig = strategy.on_bar(state)
            if sig is None or sig.side == "flat":
                continue
            open_pos = _OpenPosition(
                entry_ts=state.timestamp,
                side=sig.side,
                size_usd=sig.size_usd,
                entry_px=state.mid,
                entry_ipd=state.ipd,
                entry_mid=state.mid,
                target_pnl_bps=sig.expected_pnl_bps,
                metadata=sig.metadata,
            )

        # 最後に未決済が残れば最終バーで強制 exit
        if open_pos is not None and states:
            final_state = states[-1]
            trades.append(self._close(open_pos, final_state))

        return self._summarize(strategy.name, trades)

    def _close(
        self,
        pos: _OpenPosition,
        state: MarketState,
    ) -> FilledTrade:
        exit_px = state.mid
        direction = 1 if pos.side == "long" else -1
        gross = pos.size_usd * direction * (exit_px - pos.entry_px) / pos.entry_px
        holding_hours = (state.timestamp - pos.entry_ts).total_seconds() / 3600
        cost = total_cost(
            TradeCostInput(
                size_usd=pos.size_usd,
                holding_hours=max(holding_hours, 0.0),
                funding_rate=state.funding_rate,
                ipd=state.ipd,
                mid=state.mid,
                entry_ipd=pos.entry_ipd,
                entry_mid=pos.entry_mid,
                resilience_factor=1.0,  # Phase 2: 実測 resilience を入れる
            ),
            self.cost_cfg,
        )
        return FilledTrade(
            entry_ts=pos.entry_ts,
            exit_ts=state.timestamp,
            symbol=state.symbol,
            side=pos.side,
            size_usd=pos.size_usd,
            entry_px=pos.entry_px,
            exit_px=exit_px,
            gross_pnl_usd=gross,
            cost_usd=cost,
            net_pnl_usd=gross - cost,
            holding_minutes=holding_hours * 60,
            metadata=pos.metadata,
        )

    def _summarize(
        self,
        name: str,
        trades: list[FilledTrade],
    ) -> BacktestResult:
        if not trades:
            return BacktestResult(
                strategy_name=name,
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
        gross = sum(t.gross_pnl_usd for t in trades)
        cost = sum(t.cost_usd for t in trades)
        net = gross - cost

        # 1取引あたり net return (bps)
        net_bps = [(t.net_pnl_usd / max(t.size_usd, 1e-9)) * 10000.0 for t in trades]
        mean_b = sum(net_bps) / len(net_bps)
        var_b = sum((x - mean_b) ** 2 for x in net_bps) / len(net_bps)
        std_b = math.sqrt(var_b) if var_b > 0 else 0.0
        se_b = std_b / math.sqrt(len(net_bps)) if net_bps else 0.0
        wins = sum(1 for x in net_bps if x > 0)

        return BacktestResult(
            strategy_name=name,
            n_trades=len(trades),
            gross_pnl_usd=gross,
            cost_usd=cost,
            net_pnl_usd=net,
            mean_net_bps=mean_b,
            std_net_bps=std_b,
            se_bps=se_b,
            win_rate=wins / len(trades),
            trades=trades,
        )
