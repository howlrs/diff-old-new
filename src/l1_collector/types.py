"""L1 共通型 (Pydantic)."""

from __future__ import annotations

from datetime import datetime

from pydantic import BaseModel, Field


class L2BookLevel(BaseModel):
    """板の1レベル."""

    px: float
    sz: float
    n: int = 0  # 板に並ぶ注文数


class L2BookSnapshot(BaseModel):
    """ある時刻の板スナップショット (top N levels)."""

    symbol: str
    exchange_ts: datetime  # exchange (HL) 提供のイベント時刻 - Gemini指摘により必須
    recv_ts: datetime  # ローカル受信時刻
    bids: list[L2BookLevel]
    asks: list[L2BookLevel]
    sequence: int | None = None  # WS の seq number (gap detection)
    is_recovery_snapshot: bool = False  # REST 復旧由来の snapshot か

    @property
    def best_bid(self) -> float | None:
        return self.bids[0].px if self.bids else None

    @property
    def best_ask(self) -> float | None:
        return self.asks[0].px if self.asks else None

    @property
    def mid(self) -> float | None:
        if self.best_bid is None or self.best_ask is None:
            return None
        return (self.best_bid + self.best_ask) / 2


class TradeEvent(BaseModel):
    """単一の約定."""

    symbol: str
    exchange_ts: datetime
    recv_ts: datetime
    px: float
    sz: float
    side: str  # "B" (buy taker) / "A" (sell taker)
    trade_id: str | int | None = None


class AssetCtx(BaseModel):
    """meta / metaAndAssetCtxs の銘柄コンテキスト (1分間隔 polling)."""

    symbol: str
    poll_ts: datetime
    mark_px: float | None = None
    oracle_px: float | None = None
    funding_rate: float | None = None  # 現在の funding rate
    open_interest: float | None = None
    day_volume: float | None = None
    impact_pxs: tuple[float, float] | None = None  # (impactBid, impactAsk) if available


class GapEvent(BaseModel):
    """シーケンス不整合・WS切断検出 (gap_recovery が消費)."""

    symbol: str
    detected_ts: datetime
    last_seq: int | None
    new_seq: int | None
    reason: str = Field(description="seq_jump / ws_disconnect / heartbeat_timeout")
