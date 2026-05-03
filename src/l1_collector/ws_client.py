"""Hyperliquid WebSocket client (l2book + trades).

dry-run (2026-05-04) で判明した実仕様:
- coin は HIP-3 を含めて単一文字列 (例: "xyz:SP500", "BTC")
- l2Book / trades に seq number は無い → 時間ベースの health check に変更
- price/size は string ("7245.9") で返る → Pydantic 側で float coerce

Gemini指摘反映:
- exchange_ts (event time) と recv_ts (受信時刻) を別カラム
- WS切断・heartbeat 超過時に GapEvent emit (上位 gap_recovery が REST snapshot 取得)
- 自動再接続 (指数バックオフ + jitter)
- 安定稼働 (≥ stable_uptime_sec) で attempt reset (Bug 1)
"""

from __future__ import annotations

import asyncio
import json
import random
from collections.abc import AsyncIterator
from dataclasses import dataclass
from datetime import UTC, datetime

import websockets
from websockets.exceptions import ConnectionClosed

from src.config import HyperliquidConfig
from src.l1_collector.types import (
    GapEvent,
    L2BookLevel,
    L2BookSnapshot,
    TradeEvent,
)
from src.logging_setup import get_logger

log = get_logger("l1.ws")


@dataclass
class _SymbolState:
    last_recv_ts: datetime | None = None


class HLWebSocketClient:
    """非同期 HL WS クライアント (購読 + heartbeat + reconnect).

    使い方:
        client = HLWebSocketClient(cfg)  # 既定で all_symbols を購読
        async for event in client.stream():
            # event は L2BookSnapshot | TradeEvent | GapEvent
            ...
    """

    def __init__(
        self,
        cfg: HyperliquidConfig,
        symbols: list[str] | None = None,
    ) -> None:
        self.cfg = cfg
        self.symbols = symbols if symbols is not None else cfg.all_symbols
        self._states: dict[str, _SymbolState] = {s: _SymbolState() for s in self.symbols}

    async def stream(
        self,
    ) -> AsyncIterator[L2BookSnapshot | TradeEvent | GapEvent]:
        """無限ストリーム. 切断は自動再接続で吸収.

        Bug 1 修正: 30 秒安定稼働後に attempt をリセット.
        """
        attempt = 0
        stable_uptime_sec = self.cfg.ws_stable_uptime_sec
        while attempt < self.cfg.ws_reconnect_max_attempts:
            connected_at: float | None = None
            try:
                loop = asyncio.get_running_loop()
                connected_at = loop.time()
                async for event in self._connect_and_stream():
                    yield event
                    if (
                        attempt > 0
                        and connected_at is not None
                        and (loop.time() - connected_at) >= stable_uptime_sec
                    ):
                        attempt = 0
                        connected_at = None
            except ConnectionClosed as exc:
                attempt += 1
                wait = min(
                    self.cfg.ws_reconnect_backoff_initial_sec * (2**attempt),
                    self.cfg.ws_reconnect_backoff_max_sec,
                )
                wait *= 0.5 + random.random()  # jitter
                log.warning(
                    "ws.disconnected",
                    attempt=attempt,
                    wait=wait,
                    code=getattr(exc, "code", None),
                )
                # 切断時は GapEvent を上位へ流す (recovery 検出用)
                for sym, st in self._states.items():
                    yield GapEvent(
                        symbol=sym,
                        detected_ts=datetime.now(UTC),
                        last_seen_ts=st.last_recv_ts,
                        reason="ws_disconnect",
                    )
                await asyncio.sleep(wait)
            except Exception as exc:
                attempt += 1
                log.error("ws.error", error=str(exc), attempt=attempt)
                await asyncio.sleep(2.0)

    async def _connect_and_stream(
        self,
    ) -> AsyncIterator[L2BookSnapshot | TradeEvent | GapEvent]:
        async with websockets.connect(self.cfg.ws_url) as ws:
            log.info("ws.connected", url=self.cfg.ws_url, symbols=self.symbols)

            for sym in self.symbols:
                await ws.send(
                    json.dumps(
                        {
                            "method": "subscribe",
                            "subscription": {"type": "l2Book", "coin": sym},
                        }
                    )
                )
                await ws.send(
                    json.dumps(
                        {
                            "method": "subscribe",
                            "subscription": {"type": "trades", "coin": sym},
                        }
                    )
                )

            async for raw_msg in ws:
                recv_ts = datetime.now(UTC)
                try:
                    msg = json.loads(raw_msg)
                except json.JSONDecodeError:
                    log.warning("ws.bad_json", raw=raw_msg[:200])
                    continue

                channel = msg.get("channel")
                if channel == "l2Book":
                    snapshot = self._parse_l2book(msg, recv_ts)
                    if snapshot is not None:
                        self._states.setdefault(
                            snapshot.symbol, _SymbolState()
                        ).last_recv_ts = recv_ts
                        yield snapshot
                elif channel == "trades":
                    for trade in self._parse_trades(msg, recv_ts):
                        self._states.setdefault(trade.symbol, _SymbolState()).last_recv_ts = recv_ts
                        yield trade
                # subscriptionResponse などは無視

    def _parse_l2book(
        self,
        msg: dict,
        recv_ts: datetime,
    ) -> L2BookSnapshot | None:
        data = msg.get("data") or {}
        coin = data.get("coin") or msg.get("coin")
        if coin is None:
            return None

        ts_field = data.get("time")
        exchange_ts = (
            datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC)
            if ts_field is not None
            else recv_ts
        )

        levels = data.get("levels") or [[], []]
        bids_raw = levels[0][: self.cfg.l2book_levels] if len(levels) > 0 else []
        asks_raw = levels[1][: self.cfg.l2book_levels] if len(levels) > 1 else []
        bids = [L2BookLevel(**lv) for lv in bids_raw]
        asks = [L2BookLevel(**lv) for lv in asks_raw]

        return L2BookSnapshot(
            symbol=coin,
            exchange_ts=exchange_ts,
            recv_ts=recv_ts,
            bids=bids,
            asks=asks,
        )

    def _parse_trades(
        self,
        msg: dict,
        recv_ts: datetime,
    ) -> list[TradeEvent]:
        out: list[TradeEvent] = []
        data = msg.get("data") or []
        if isinstance(data, dict):
            data = [data]
        for t in data:
            ts_field = t.get("time")
            exchange_ts = (
                datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC)
                if ts_field is not None
                else recv_ts
            )
            users = t.get("users") or [None, None]
            buyer = users[0] if isinstance(users, list) and len(users) > 0 else None
            seller = users[1] if isinstance(users, list) and len(users) > 1 else None
            try:
                trade = TradeEvent(
                    symbol=t.get("coin") or msg.get("coin", ""),
                    exchange_ts=exchange_ts,
                    recv_ts=recv_ts,
                    px=t.get("px"),
                    sz=t.get("sz"),
                    side=t.get("side", ""),
                    trade_id=t.get("tid"),
                    buyer=buyer,
                    seller=seller,
                    hash=t.get("hash"),
                )
            except Exception as exc:
                log.warning("ws.trade_parse_failed", error=str(exc), raw=str(t)[:200])
                continue
            out.append(trade)
        return out
