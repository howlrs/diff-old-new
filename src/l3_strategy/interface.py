"""Strategy Interface: backtest と live が同一基底を継承する.

Geminiの最重要指摘 (実装乖離防止):
    BacktestStrategy も LiveStrategy も同一の Strategy ABC を継承する.
    シグナル生成ロジックは1箇所だけに存在する.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from typing import Literal

import polars as pl

Side = Literal["long", "short", "flat"]


@dataclass
class MarketState:
    """Strategy が実行時に参照する snapshot."""

    timestamp: datetime
    symbol: str
    mid: float
    funding_rate: float | None
    impact_bid: float | None
    impact_ask: float | None
    ipd: float | None
    regime: str
    regime_uncertain: bool
    extras: dict = field(default_factory=dict)


@dataclass
class Signal:
    """戦略が出力する trade intent."""

    timestamp: datetime
    symbol: str
    side: Side
    size_usd: float
    expected_pnl_bps: float
    confidence: float = 1.0
    metadata: dict = field(default_factory=dict)


@dataclass
class FilledTrade:
    """backtest engine が約定として記録するレコード."""

    entry_ts: datetime
    exit_ts: datetime
    symbol: str
    side: Side
    size_usd: float
    entry_px: float
    exit_px: float
    gross_pnl_usd: float
    cost_usd: float
    net_pnl_usd: float
    holding_minutes: float
    metadata: dict = field(default_factory=dict)


class Strategy(ABC):
    """全 Strategy の基底.

    backtest と live が同一を継承.
    """

    name: str = "abstract"

    @abstractmethod
    def on_bar(self, state: MarketState) -> Signal | None:
        """各バーで呼ばれる. シグナルを出すなら Signal, 出さないなら None.

        regime_uncertain=true のバーは engine 側で skip するため,
        Strategy 実装は通常 regime チェック不要 (defense in depth で確認しても良い).
        """
        raise NotImplementedError

    def warmup_bars(self) -> int:
        """ウォームアップに必要なバー数 (engine が drop)."""
        return 0


def df_to_market_states(df: pl.DataFrame) -> list[MarketState]:
    """L2 features DataFrame を MarketState の list に変換 (簡易版)."""
    states: list[MarketState] = []
    for row in df.iter_rows(named=True):
        states.append(
            MarketState(
                timestamp=row.get("exchange_ts"),
                symbol=row.get("symbol", ""),
                mid=row.get("mid") or 0.0,
                funding_rate=row.get("funding_rate"),
                impact_bid=row.get("impact_bid_px"),
                impact_ask=row.get("impact_ask_px"),
                ipd=row.get("ipd"),
                regime=row.get("regime") or "",
                regime_uncertain=bool(row.get("regime_uncertain") or False),
                extras={k: v for k, v in row.items() if k.startswith("ema_")},
            )
        )
    return states
