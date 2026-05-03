"""BacktestEngine: L2 features を時系列で Strategy.on_bar に渡し,
コスト控除後 P/L と LLN/CLT 統計を集計する.

Issue #29 反映: マルチポジ + 複数銘柄対応.
- _open_positions は symbol → list[_OpenPosition] (per-symbol ledger)
- max_positions_per_symbol で銘柄毎の同時ポジ数を制限
- max_total_positions でグローバル制限
- 全銘柄の bars を timestamp 順に並べ, symbol ごとに on_bar を呼ぶ
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
    """エントリー時のマーケットスナップショット."""

    entry_ts: datetime
    symbol: str
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
    se_bps: float  # standard error = std / sqrt(N) <- CLT
    win_rate: float
    trades: list[FilledTrade]
    by_symbol: dict[str, dict[str, float]] = field(default_factory=dict)


class BacktestEngine:
    """マルチポジション・マルチ銘柄バックテスト engine.

    各 symbol で独立に open positions を管理. exit は holding_minutes timeout のみ
    (Phase 2 で動的 exit 規律に拡張).
    """

    def __init__(
        self,
        cost_cfg: CostConfig,
        *,
        max_positions_per_symbol: int = 1,
        max_total_positions: int = 10,
    ) -> None:
        self.cost_cfg = cost_cfg
        self.max_positions_per_symbol = max_positions_per_symbol
        self.max_total_positions = max_total_positions

    def run(
        self,
        strategy: Strategy,
        df: pl.DataFrame,
        *,
        symbol_filter: str | None = None,
        symbols: list[str] | None = None,
        exit_after_minutes: int = 60,
    ) -> BacktestResult:
        """全 symbol の bars を timestamp 順に処理.

        Args:
            symbol_filter: 1銘柄だけにフィルタしたい場合.
            symbols: 並行処理対象の symbol 集合 (None = df 内全 symbol).
        """
        if symbol_filter:
            df = df.filter(pl.col("symbol") == symbol_filter)
        df = df.sort("exchange_ts")
        states = df_to_market_states(df)
        target_symbols = (
            set(symbols) if symbols is not None else {s.symbol for s in states if s.symbol}
        )
        log.info(
            "backtest.start",
            strategy=strategy.name,
            n_bars=len(states),
            n_symbols=len(target_symbols),
            max_per_symbol=self.max_positions_per_symbol,
        )

        trades: list[FilledTrade] = []
        open_positions: dict[str, list[_OpenPosition]] = {}

        for state in states:
            if state.regime_uncertain:
                continue
            if state.symbol not in target_symbols:
                continue

            # 1) このバーの symbol の既存ポジを exit 判定
            current_open = open_positions.get(state.symbol, [])
            keep: list[_OpenPosition] = []
            for pos in current_open:
                holding_min = (state.timestamp - pos.entry_ts).total_seconds() / 60
                if holding_min >= exit_after_minutes:
                    trades.append(self._close(pos, state))
                else:
                    keep.append(pos)
            if keep:
                open_positions[state.symbol] = keep
            else:
                open_positions.pop(state.symbol, None)

            # 2) 容量チェック
            n_total = sum(len(v) for v in open_positions.values())
            n_for_symbol = len(open_positions.get(state.symbol, []))
            if n_total >= self.max_total_positions or n_for_symbol >= self.max_positions_per_symbol:
                continue

            # 3) シグナル取得
            sig = strategy.on_bar(state)
            if sig is None or sig.side == "flat":
                continue
            new_pos = _OpenPosition(
                entry_ts=state.timestamp,
                symbol=state.symbol,
                side=sig.side,
                size_usd=sig.size_usd,
                entry_px=state.mid,
                entry_ipd=state.ipd,
                entry_mid=state.mid,
                target_pnl_bps=sig.expected_pnl_bps,
                metadata=sig.metadata,
            )
            open_positions.setdefault(state.symbol, []).append(new_pos)

        # 全未決済を最終バーで強制 exit
        if states:
            final_state_by_symbol: dict[str, MarketState] = {}
            for s in reversed(states):
                final_state_by_symbol.setdefault(s.symbol, s)
            for symbol, positions in open_positions.items():
                fs = final_state_by_symbol.get(symbol)
                if fs is None:
                    continue
                for pos in positions:
                    trades.append(self._close(pos, fs))

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
                resilience_factor=1.0,  # Phase 2: per-symbol 実測 resilience
            ),
            self.cost_cfg,
        )
        return FilledTrade(
            entry_ts=pos.entry_ts,
            exit_ts=state.timestamp,
            symbol=pos.symbol,
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
                by_symbol={},
            )
        gross = sum(t.gross_pnl_usd for t in trades)
        cost = sum(t.cost_usd for t in trades)
        net = gross - cost
        net_bps = [(t.net_pnl_usd / max(t.size_usd, 1e-9)) * 10000.0 for t in trades]
        mean_b = sum(net_bps) / len(net_bps)
        var_b = sum((x - mean_b) ** 2 for x in net_bps) / len(net_bps)
        std_b = math.sqrt(var_b) if var_b > 0 else 0.0
        se_b = std_b / math.sqrt(len(net_bps)) if net_bps else 0.0
        wins = sum(1 for x in net_bps if x > 0)

        # symbol 別集計
        by_symbol: dict[str, dict[str, float]] = {}
        for t in trades:
            entry = by_symbol.setdefault(
                t.symbol,
                {
                    "n": 0.0,
                    "gross_pnl_usd": 0.0,
                    "cost_usd": 0.0,
                    "net_pnl_usd": 0.0,
                    "wins": 0.0,
                },
            )
            entry["n"] += 1
            entry["gross_pnl_usd"] += t.gross_pnl_usd
            entry["cost_usd"] += t.cost_usd
            entry["net_pnl_usd"] += t.net_pnl_usd
            if t.net_pnl_usd > 0:
                entry["wins"] += 1
        for _sym, e in by_symbol.items():
            e["win_rate"] = e["wins"] / e["n"] if e["n"] > 0 else 0.0

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
            by_symbol=by_symbol,
        )
