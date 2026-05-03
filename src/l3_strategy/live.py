"""LiveEngine (dry-run) — Issue #27.

Strategy ABC を継承する戦略を実 WS / REST から流れる MarketState で駆動.
本実装は **dry-run mode のみ**: 実際の発注はせず, Signal をログ出力.

Phase 3 で:
- EIP-712 hot wallet 署名 (hyperliquid-python-sdk Exchange)
- Order lifecycle 追跡
- Kill switch (regime境界 -15min, drawdown 上限)
を別 issue で追加する.

設計原則 (Gemini partner 最重要指摘):
    BacktestStrategy と LiveStrategy は同一 Strategy ABC を継承する.
    シグナル生成ロジックは1箇所のみ.
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
from src.l1_collector.types import AssetCtx, GapEvent, L2BookSnapshot, TradeEvent
from src.l1_collector.ws_client import HLWebSocketClient
from src.l2_features.regime import classify_regime, is_near_boundary
from src.l3_strategy.interface import MarketState, Strategy
from src.logging_setup import get_logger

log = get_logger("l3.live")


class LiveEngine:
    """WS+REST から MarketState を作って Strategy.on_bar に流す dry-run engine.

    実発注はしない. Signal はログ出力のみ.
    """

    def __init__(
        self,
        cfg: AppConfig,
        strategy: Strategy,
        *,
        dry_run: bool = True,
    ) -> None:
        if not dry_run:
            raise NotImplementedError(
                "Live execution は Phase 3 で実装. 現状は dry_run=True のみサポート."
            )
        self.cfg = cfg
        self.strategy = strategy
        self.dry_run = dry_run
        self.ws = HLWebSocketClient(cfg.hyperliquid)
        self.rest = HLRestClient(cfg.hyperliquid)
        self.gap = GapRecovery(self.rest)

        # 銘柄ごとの最新 ctx (oracle / funding / impact) cache
        self._latest_ctx: dict[str, AssetCtx] = {}
        self._stop = asyncio.Event()
        self._signal_count: dict[str, int] = defaultdict(int)
        self._gap_tasks: set[asyncio.Task] = set()

    def request_stop(self) -> None:
        log.warning("live.shutdown_requested")
        self._stop.set()

    async def run(self) -> None:
        tasks = [
            asyncio.create_task(self._run_ws(), name="ws"),
            asyncio.create_task(self._run_rest_poll(), name="rest"),
            asyncio.create_task(self._run_heartbeat(), name="heartbeat"),
        ]
        try:
            await self._stop.wait()
        finally:
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            if self._gap_tasks:
                await asyncio.gather(*self._gap_tasks, return_exceptions=True)
            await self.rest.close()
            log.info("live.stopped", signals=dict(self._signal_count))

    async def _run_ws(self) -> None:
        try:
            async for ev in self.ws.stream():
                if self._stop.is_set():
                    return
                if isinstance(ev, L2BookSnapshot):
                    await self._handle_l2(ev)
                elif isinstance(ev, TradeEvent):
                    pass  # 戦略が必要なら拡張
                elif isinstance(ev, GapEvent):
                    task = asyncio.create_task(self._recover(ev))
                    self._gap_tasks.add(task)
                    task.add_done_callback(self._gap_tasks.discard)
        except asyncio.CancelledError:
            return

    async def _recover(self, ev: GapEvent) -> None:
        with contextlib.suppress(Exception):
            await self.gap.recover(ev)

    async def _run_rest_poll(self) -> None:
        try:
            async for ctxs in self.rest.stream_asset_ctxs():
                if self._stop.is_set():
                    return
                for c in ctxs:
                    self._latest_ctx[c.symbol] = c
        except asyncio.CancelledError:
            return

    async def _handle_l2(self, snap: L2BookSnapshot) -> None:
        ctx = self._latest_ctx.get(snap.symbol)
        if ctx is None or snap.mid is None:
            return
        regime = classify_regime(snap.exchange_ts, self.cfg.regime).value
        regime_uncertain = is_near_boundary(snap.exchange_ts, self.cfg.regime)
        ipd = self._compute_ipd(snap, ctx)
        state = MarketState(
            timestamp=snap.exchange_ts,
            symbol=snap.symbol,
            mid=snap.mid,
            funding_rate=ctx.funding_rate,
            impact_bid=ctx.impact_bid_px,
            impact_ask=ctx.impact_ask_px,
            ipd=ipd,
            regime=regime,
            regime_uncertain=regime_uncertain,
            extras={},  # Phase 2 で btc_ret_cum 等を入れる
        )
        sig = self.strategy.on_bar(state)
        if sig is not None and sig.side != "flat":
            self._signal_count[sig.symbol] += 1
            log.info(
                "live.signal",
                strategy=self.strategy.name,
                symbol=sig.symbol,
                side=sig.side,
                size_usd=sig.size_usd,
                expected_pnl_bps=sig.expected_pnl_bps,
                confidence=sig.confidence,
                regime=regime,
                metadata=sig.metadata,
            )
            # dry_run = True なのでここで実発注しない

    def _compute_ipd(self, snap: L2BookSnapshot, ctx: AssetCtx) -> float | None:
        """簡易 IPD: ctx の impact_bid/ask と snap.mid から."""
        if ctx.impact_bid_px is None or ctx.impact_ask_px is None or snap.mid is None:
            return None
        s = snap.mid
        return max(ctx.impact_bid_px - s, 0.0) - max(s - ctx.impact_ask_px, 0.0)

    async def _run_heartbeat(self) -> None:
        interval = self.cfg.logging.heartbeat_interval_sec
        try:
            while not self._stop.is_set():
                with contextlib.suppress(TimeoutError):
                    await asyncio.wait_for(self._stop.wait(), timeout=interval)
                log.info(
                    "live.heartbeat",
                    strategy=self.strategy.name,
                    signals=dict(self._signal_count),
                    n_ctxs=len(self._latest_ctx),
                )
        except asyncio.CancelledError:
            return


def _install_signal_handlers(loop: asyncio.AbstractEventLoop, engine: LiveEngine) -> None:
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, engine.request_stop)
        except NotImplementedError:
            signal.signal(sig, lambda *_: engine.request_stop())


# 未使用 import 警告抑制 (datetime/UTC/timedelta は将来の kill-switch 等で使う)
_ = (datetime, UTC, timedelta, deque)
