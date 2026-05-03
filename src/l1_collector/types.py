"""L1 共通型 (Pydantic v2).

dry-run (2026-05-04) で判明した実 HL API の仕様:
- l2Book に seq number は存在しない (時刻のみ)
- price/size は string ("7245.9") で返される → Pydantic で float に coerce
- trades には users (buyer/seller wallet address) が含まれる
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, field_validator


def _coerce_float_opt(value: Any) -> float | None:
    """空・None は None, それ以外は float."""
    if value is None or value == "":
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _coerce_float_required(value: Any) -> float:
    """必須 float (None や parse 不能なら 0.0).

    Gemini指摘 (Bug A) 反映: float 型のフィールドに None が来ると ValidationError で
    プロセスがクラッシュするため, 板情報の必須フィールドは 0.0 に倒す.
    """
    v = _coerce_float_opt(value)
    return 0.0 if v is None else v


class L2BookLevel(BaseModel):
    """板の1レベル. HL は string で返すので float に変換."""

    model_config = ConfigDict(extra="ignore")

    px: float
    sz: float
    n: int = 0  # 板に並ぶ注文数

    @field_validator("px", "sz", mode="before")
    @classmethod
    def _to_float(cls, v: Any) -> float:
        return _coerce_float_required(v)


class L2BookSnapshot(BaseModel):
    """ある時刻の板スナップショット (top N levels)."""

    model_config = ConfigDict(extra="ignore")

    symbol: str
    exchange_ts: datetime  # exchange (HL) 提供のイベント時刻
    recv_ts: datetime  # ローカル受信時刻
    bids: list[L2BookLevel]
    asks: list[L2BookLevel]
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

    model_config = ConfigDict(extra="ignore")

    symbol: str
    exchange_ts: datetime
    recv_ts: datetime
    px: float
    sz: float
    side: str  # "B" (buy taker) / "A" (sell taker)
    trade_id: str | int | None = None
    buyer: str | None = None
    seller: str | None = None
    hash_: str | None = Field(default=None, alias="hash")

    @field_validator("px", "sz", mode="before")
    @classmethod
    def _to_float(cls, v: Any) -> float:
        return _coerce_float_required(v)


class AssetCtx(BaseModel):
    """meta / metaAndAssetCtxs の銘柄コンテキスト (1分間隔 polling)."""

    model_config = ConfigDict(extra="ignore")

    symbol: str
    poll_ts: datetime
    dex: str = ""  # "" = core, "xyz" = HIP-3 Trade[XYZ]
    mark_px: float | None = None
    oracle_px: float | None = None
    mid_px: float | None = None
    funding_rate: float | None = None
    premium: float | None = None
    open_interest: float | None = None
    day_volume: float | None = None
    day_base_volume: float | None = None
    prev_day_px: float | None = None
    impact_bid_px: float | None = None
    impact_ask_px: float | None = None


class GapEvent(BaseModel):
    """WS切断や受信間隔超過の検出 (gap_recovery が消費).

    HL は seq number を提供しないため, 時間ベースのギャップ検出に変更.
    """

    symbol: str
    detected_ts: datetime
    last_seen_ts: datetime | None
    silence_sec: float | None = None
    reason: str = Field(description="ws_disconnect / heartbeat_timeout")
