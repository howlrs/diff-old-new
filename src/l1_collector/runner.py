"""L1 統合 runner: WS + REST + storage を組み合わせた1プロセス.

使い方:
    python -m src.l1_collector

挙動:
- WS から l2book / trades をストリーム受信、5秒ごとにスナップショットを Parquet 書き出し
- REST から 1分ごとに metaAndAssetCtxs → AssetCtx Parquet 書き出し
- gap event は gap_recovery で REST snapshot 取得して raw に追加保存
- 5分ごと heartbeat ログ
"""

from __future__ import annotations

import asyncio
import contextlib
from collections import defaultdict, deque
from datetime import UTC, datetime, timedelta

from src.config import AppConfig
from src.l1_collector.gap_recovery import GapRecovery
from src.l1_collector.rest_client import HLRestClient
from src.l1_collector.storage import write_parquet_atomic
from src.l1_collector.types import (
    AssetCtx,
    GapEvent,
    L2BookSnapshot,
    TradeEvent,
)
from src.l1_collector.ws_client import HLWebSocketClient
from src.logging_setup import get_logger

log = get_logger("l1.runner")


def _l2_to_row(s: L2BookSnapshot) -> dict:
    return {
        "symbol": s.symbol,
        "exchange_ts": s.exchange_ts,
        "recv_ts": s.recv_ts,
        "sequence": s.sequence,
        "is_recovery_snapshot": s.is_recovery_snapshot,
        "best_bid": s.best_bid,
        "best_ask": s.best_ask,
        "mid": s.mid,
        "bid_pxs": [b.px for b in s.bids],
        "bid_szs": [b.sz for b in s.bids],
        "ask_pxs": [a.px for a in s.asks],
        "ask_szs": [a.sz for a in s.asks],
    }


def _trade_to_row(t: TradeEvent) -> dict:
    return {
        "symbol": t.symbol,
        "exchange_ts": t.exchange_ts,
        "recv_ts": t.recv_ts,
        "px": t.px,
        "sz": t.sz,
        "side": t.side,
        "trade_id": str(t.trade_id) if t.trade_id is not None else None,
    }


def _ctx_to_row(c: AssetCtx) -> dict:
    return {
        "symbol": c.symbol,
        "poll_ts": c.poll_ts,
        "mark_px": c.mark_px,
        "oracle_px": c.oracle_px,
        "funding_rate": c.funding_rate,
        "open_interest": c.open_interest,
        "day_volume": c.day_volume,
        "impact_bid_px": c.impact_pxs[0] if c.impact_pxs else None,
        "impact_ask_px": c.impact_pxs[1] if c.impact_pxs else None,
    }


class L1Runner:
    """WS + REST + storage を統合した1プロセス runner."""

    def __init__(self, cfg: AppConfig) -> None:
        self.cfg = cfg
        self.ws = HLWebSocketClient(cfg.hyperliquid)
        self.rest = HLRestClient(cfg.hyperliquid)
        self.gap = GapRecovery(self.rest)

        # メモリバッファ (定期 flush)
        self._l2_buf: deque[dict] = deque()
        self._trade_buf: deque[dict] = deque()
        self._ctx_buf: deque[dict] = deque()

        # heartbeat 用カウンタ
        self._counts: dict[str, int] = defaultdict(int)
        self._last_seen: dict[str, datetime] = {}

    async def run(self) -> None:
        """並列タスクを起動して停止信号を待つ."""
        try:
            await asyncio.gather(
                self._run_ws(),
                self._run_rest_poll(),
                self._run_flusher(),
                self._run_heartbeat(),
            )
        finally:
            await self.rest.close()

    async def _run_ws(self) -> None:
        async for ev in self.ws.stream():
            self._counts[type(ev).__name__] += 1
            if isinstance(ev, L2BookSnapshot):
                self._l2_buf.append(_l2_to_row(ev))
                self._last_seen[ev.symbol] = ev.recv_ts
            elif isinstance(ev, TradeEvent):
                self._trade_buf.append(_trade_to_row(ev))
                self._last_seen[ev.symbol] = ev.recv_ts
            elif isinstance(ev, GapEvent):
                snap = await self.gap.recover(ev)
                if snap is not None:
                    self._l2_buf.append(_l2_to_row(snap))

    async def _run_rest_poll(self) -> None:
        async for ctxs in self.rest.stream_asset_ctxs():
            for c in ctxs:
                self._ctx_buf.append(_ctx_to_row(c))
                self._counts["AssetCtx"] += 1

    async def _run_flusher(self) -> None:
        """1分ごとにバッファを Parquet に flush."""
        while True:
            await asyncio.sleep(60)
            await self._flush_all()

    async def _flush_all(self) -> None:
        """Gemini指摘 (Bug 4) 反映: deque を新オブジェクトに swap してから flush.

        list(buf); buf.clear() の方式では理屈上書き手とのレースが残る.
        新 deque に挿げ替えてから旧 deque を処理することで完全に分離する.
        """
        # L2
        old_l2, self._l2_buf = self._l2_buf, deque()
        if old_l2:
            write_parquet_atomic(list(old_l2), "l2book", self.cfg.storage)
        # trades
        old_trades, self._trade_buf = self._trade_buf, deque()
        if old_trades:
            write_parquet_atomic(list(old_trades), "trades", self.cfg.storage)
        # ctxs
        old_ctxs, self._ctx_buf = self._ctx_buf, deque()
        if old_ctxs:
            write_parquet_atomic(list(old_ctxs), "asset_ctxs", self.cfg.storage)

    async def _run_heartbeat(self) -> None:
        interval = self.cfg.logging.heartbeat_interval_sec
        threshold = timedelta(minutes=10)
        while True:
            await asyncio.sleep(interval)
            now = datetime.now(UTC)
            stale: list[str] = []
            for sym, last in self._last_seen.items():
                if now - last > threshold:
                    stale.append(sym)
            log.info(
                "heartbeat",
                counts=dict(self._counts),
                stale_symbols=stale,
                buffer_l2=len(self._l2_buf),
                buffer_trades=len(self._trade_buf),
                buffer_ctxs=len(self._ctx_buf),
            )


async def _amain(cfg: AppConfig) -> None:
    runner = L1Runner(cfg)
    log.info("l1.start", symbols=cfg.hyperliquid.symbols)
    with contextlib.suppress(KeyboardInterrupt):
        await runner.run()
    await runner._flush_all()
    log.info("l1.stop")


def main() -> None:
    from pathlib import Path

    from src.config import load_config
    from src.logging_setup import setup_logging

    cfg = load_config(
        [
            Path("config/default.yaml"),
            Path("config/local.yaml"),
        ]
    )
    setup_logging(cfg.logging)
    asyncio.run(_amain(cfg))


if __name__ == "__main__":
    main()
