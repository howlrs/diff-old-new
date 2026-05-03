"""Hyperliquid WebSocket client (l2book + trades).

Gemini指摘反映:
- exchange_ts (event time) と recv_ts (受信時刻) を別カラム
- l2book sequence 不整合検出 → GapEvent emit (上位の gap_recovery が snapshot 取得)
- 自動再接続 (指数バックオフ)
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
    last_seq: int | None = None
    last_recv_ts: datetime | None = None


class HLWebSocketClient:
    """非同期 HL WS クライアント (購読 + sequence check + reconnect).

    使い方:
        client = HLWebSocketClient(cfg, symbols=["SP500", "BTC"])
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
        self.symbols = symbols or cfg.symbols
        self._states: dict[str, _SymbolState] = {s: _SymbolState() for s in self.symbols}

    async def stream(
        self,
    ) -> AsyncIterator[L2BookSnapshot | TradeEvent | GapEvent]:
        """無限ストリーム. 切断は自動再接続で吸収.

        Gemini指摘 (Bug 1) 反映: 1件受信で attempt=0 にすると不安定環境で
        backoff が全く効かない. 安定稼働 (≥ stable_uptime_sec) してから reset.
        """
        attempt = 0
        stable_uptime_sec = 30.0
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
                        connected_at = None  # 一度だけ reset
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
                # 切断時は全 symbol 状態を reset し、上位に GapEvent を流す
                for sym, st in self._states.items():
                    yield GapEvent(
                        symbol=sym,
                        detected_ts=datetime.now(UTC),
                        last_seq=st.last_seq,
                        new_seq=None,
                        reason="ws_disconnect",
                    )
                    st.last_seq = None
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

            # subscribe (HL の subscription protocol)
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
                msg = json.loads(raw_msg)
                # subscribe ack 等は無視
                channel = msg.get("channel")
                if channel == "l2Book":
                    snapshot = self._parse_l2book(msg, recv_ts)
                    if snapshot is None:
                        continue
                    gap = self._detect_gap(snapshot)
                    if gap is not None:
                        yield gap
                    yield snapshot
                elif channel == "trades":
                    for trade in self._parse_trades(msg, recv_ts):
                        yield trade

    def _parse_l2book(
        self,
        msg: dict,
        recv_ts: datetime,
    ) -> L2BookSnapshot | None:
        data = msg.get("data") or {}
        coin = data.get("coin") or msg.get("coin")
        if coin is None:
            return None

        # HL は ms epoch を返すことが多い
        ts_field = data.get("time") or msg.get("time")
        if ts_field is None:
            exchange_ts = recv_ts
        else:
            exchange_ts = datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC)

        levels = data.get("levels") or [[], []]
        bids = [L2BookLevel(**lv) for lv in levels[0][: self.cfg.l2book_levels]]
        asks = [L2BookLevel(**lv) for lv in levels[1][: self.cfg.l2book_levels]]

        return L2BookSnapshot(
            symbol=coin,
            exchange_ts=exchange_ts,
            recv_ts=recv_ts,
            bids=bids,
            asks=asks,
            sequence=data.get("seq") or msg.get("seq"),
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
                datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC) if ts_field else recv_ts
            )
            out.append(
                TradeEvent(
                    symbol=t.get("coin") or msg.get("coin", ""),
                    exchange_ts=exchange_ts,
                    recv_ts=recv_ts,
                    px=float(t["px"]),
                    sz=float(t["sz"]),
                    side=t.get("side", ""),
                    trade_id=t.get("tid"),
                )
            )
        return out

    def _detect_gap(self, snapshot: L2BookSnapshot) -> GapEvent | None:
        """sequence number jump を検出.

        HL の WS が seq を提供する場合、欠落を検出して上位に通知する.
        seq が無い場合はこの関数では何もしない (snapshot ベースで運用).
        """
        if snapshot.sequence is None:
            return None
        st = self._states[snapshot.symbol]
        prev = st.last_seq
        st.last_seq = snapshot.sequence
        st.last_recv_ts = snapshot.recv_ts
        if prev is None:
            return None
        if snapshot.sequence == prev + 1:
            return None
        if snapshot.sequence <= prev:
            # 重複・順序逆転は上位で warning 扱い
            return GapEvent(
                symbol=snapshot.symbol,
                detected_ts=snapshot.recv_ts,
                last_seq=prev,
                new_seq=snapshot.sequence,
                reason="seq_duplicate_or_reorder",
            )
        return GapEvent(
            symbol=snapshot.symbol,
            detected_ts=snapshot.recv_ts,
            last_seq=prev,
            new_seq=snapshot.sequence,
            reason="seq_jump",
        )
