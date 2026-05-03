"""L1 統合 runner: WS + REST + storage を組み合わせた1プロセス.

挙動:
- WS から l2book / trades をストリーム受信、約1分ごとに Parquet flush
- REST から 1分ごとに metaAndAssetCtxs (core + xyz) を取得して Parquet 書き出し
- gap event は gap_recovery で REST snapshot 取得して raw に追加保存
- 5分ごと heartbeat ログ
- SIGINT / SIGTERM で graceful shutdown (Issue #30): 残バッファを最終 flush して exit
"""

from __future__ import annotations

import asyncio
import contextlib
import signal
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
        "is_recovery_snapshot": s.is_recovery_snapshot,
        "best_bid": s.best_bid,
        "best_ask": s.best_ask,
        "mid": s.mid,
        "bid_pxs": [b.px for b in s.bids],
        "bid_szs": [b.sz for b in s.bids],
        "bid_ns": [b.n for b in s.bids],
        "ask_pxs": [a.px for a in s.asks],
        "ask_szs": [a.sz for a in s.asks],
        "ask_ns": [a.n for a in s.asks],
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
        "buyer": t.buyer,
        "seller": t.seller,
        "hash": t.hash_,
    }


def _ctx_to_row(c: AssetCtx) -> dict:
    return {
        "symbol": c.symbol,
        "poll_ts": c.poll_ts,
        "dex": c.dex,
        "mark_px": c.mark_px,
        "oracle_px": c.oracle_px,
        "mid_px": c.mid_px,
        "funding_rate": c.funding_rate,
        "premium": c.premium,
        "open_interest": c.open_interest,
        "day_volume": c.day_volume,
        "day_base_volume": c.day_base_volume,
        "prev_day_px": c.prev_day_px,
        "impact_bid_px": c.impact_bid_px,
        "impact_ask_px": c.impact_ask_px,
    }


class L1Runner:
    """WS + REST + storage を統合した1プロセス runner."""

    def __init__(self, cfg: AppConfig) -> None:
        self.cfg = cfg
        self.ws = HLWebSocketClient(cfg.hyperliquid)
        self.rest = HLRestClient(cfg.hyperliquid)
        self.gap = GapRecovery(self.rest)

        # メモリバッファ (定期 flush). swap 方式で書き込み競合を回避 (Bug 4).
        self._l2_buf: deque[dict] = deque()
        self._trade_buf: deque[dict] = deque()
        self._ctx_buf: deque[dict] = deque()

        self._counts: dict[str, int] = defaultdict(int)
        self._last_seen: dict[str, datetime] = {}
        self._stop = asyncio.Event()
        # gap recovery を fire-and-forget するためのタスク集合 (Bug B 修正)
        self._gap_tasks: set[asyncio.Task] = set()

    def request_stop(self) -> None:
        """SIGINT/SIGTERM ハンドラから呼ぶ."""
        log.warning("l1.shutdown_requested")
        self._stop.set()

    async def run(self) -> None:
        """並列タスクを起動し, stop event で全て停止 → 最終 flush."""
        tasks = [
            asyncio.create_task(self._run_ws(), name="ws"),
            asyncio.create_task(self._run_rest_poll(), name="rest"),
            asyncio.create_task(self._run_flusher(), name="flusher"),
            asyncio.create_task(self._run_heartbeat(), name="heartbeat"),
        ]
        try:
            await self._stop.wait()
        finally:
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            # 進行中の gap recovery を最大 5 秒待つ
            if self._gap_tasks:
                await asyncio.gather(*self._gap_tasks, return_exceptions=True)
            await self._flush_all()
            await self.rest.close()
            log.info("l1.stopped")

    async def _run_ws(self) -> None:
        """WS 受信ループ.

        Gemini指摘 (Bug B) 反映: GapEvent 発生時の REST recover は別タスクに
        投げる (fire-and-forget). 直列 await すると WS 受信がブロックされる.
        """
        try:
            async for ev in self.ws.stream():
                if self._stop.is_set():
                    return
                self._counts[type(ev).__name__] += 1
                if isinstance(ev, L2BookSnapshot):
                    self._l2_buf.append(_l2_to_row(ev))
                    self._last_seen[ev.symbol] = ev.recv_ts
                elif isinstance(ev, TradeEvent):
                    self._trade_buf.append(_trade_to_row(ev))
                    self._last_seen[ev.symbol] = ev.recv_ts
                elif isinstance(ev, GapEvent):
                    # 別タスクで REST snapshot 取得 → 結果を _l2_buf に追加
                    task = asyncio.create_task(self._recover_and_append(ev))
                    self._gap_tasks.add(task)
                    task.add_done_callback(self._gap_tasks.discard)
        except asyncio.CancelledError:
            return

    async def _recover_and_append(self, ev: GapEvent) -> None:
        try:
            snap = await self.gap.recover(ev)
            if snap is not None:
                self._l2_buf.append(_l2_to_row(snap))
        except Exception as exc:
            log.warning("gap.recover_task_failed", symbol=ev.symbol, error=str(exc))

    async def _run_rest_poll(self) -> None:
        try:
            async for ctxs in self.rest.stream_asset_ctxs():
                if self._stop.is_set():
                    return
                for c in ctxs:
                    self._ctx_buf.append(_ctx_to_row(c))
                    self._counts["AssetCtx"] += 1
        except asyncio.CancelledError:
            return

    async def _run_flusher(self) -> None:
        try:
            while not self._stop.is_set():
                with contextlib.suppress(TimeoutError):
                    await asyncio.wait_for(self._stop.wait(), timeout=60)
                await self._flush_all()
        except asyncio.CancelledError:
            return

    async def _flush_all(self) -> None:
        """deque を新オブジェクトに swap してから flush.

        Bug 4: deque swap で書き込み競合を回避.
        Gemini Bug C: 同期 I/O (PyArrow) は asyncio.to_thread でオフロードして
        イベントループのブロックを防ぐ.
        """
        old_l2, self._l2_buf = self._l2_buf, deque()
        old_trades, self._trade_buf = self._trade_buf, deque()
        old_ctxs, self._ctx_buf = self._ctx_buf, deque()

        coros = []
        if old_l2:
            coros.append(
                asyncio.to_thread(write_parquet_atomic, list(old_l2), "l2book", self.cfg.storage)
            )
        if old_trades:
            coros.append(
                asyncio.to_thread(
                    write_parquet_atomic,
                    list(old_trades),
                    "trades",
                    self.cfg.storage,
                )
            )
        if old_ctxs:
            coros.append(
                asyncio.to_thread(
                    write_parquet_atomic,
                    list(old_ctxs),
                    "asset_ctxs",
                    self.cfg.storage,
                )
            )
        if coros:
            await asyncio.gather(*coros, return_exceptions=True)

    async def _run_heartbeat(self) -> None:
        interval = self.cfg.logging.heartbeat_interval_sec
        threshold = timedelta(minutes=10)
        try:
            while not self._stop.is_set():
                with contextlib.suppress(TimeoutError):
                    await asyncio.wait_for(self._stop.wait(), timeout=interval)
                now = datetime.now(UTC)
                stale = [sym for sym, last in self._last_seen.items() if now - last > threshold]
                log.info(
                    "heartbeat",
                    counts=dict(self._counts),
                    stale_symbols=stale,
                    buffer_l2=len(self._l2_buf),
                    buffer_trades=len(self._trade_buf),
                    buffer_ctxs=len(self._ctx_buf),
                )
        except asyncio.CancelledError:
            return


def _install_signal_handlers(loop: asyncio.AbstractEventLoop, runner: L1Runner) -> None:
    """SIGINT / SIGTERM で graceful shutdown (Issue #30)."""
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, runner.request_stop)
        except NotImplementedError:
            # Windows 等
            signal.signal(sig, lambda *_: runner.request_stop())


async def _amain(cfg: AppConfig) -> None:
    runner = L1Runner(cfg)
    loop = asyncio.get_running_loop()
    _install_signal_handlers(loop, runner)
    log.info("l1.start", symbols=cfg.hyperliquid.all_symbols)
    with contextlib.suppress(KeyboardInterrupt):
        await runner.run()


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
