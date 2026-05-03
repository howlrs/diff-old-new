"""Hyperliquid REST poller (meta / metaAndAssetCtxs / fundingHistory).

責務:
- 1分間隔で metaAndAssetCtxs を polling → AssetCtx を produce
- fundingHistory は日次 batch で取得
- gap recovery 用の l2book snapshot 取得
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from typing import Any

import httpx

from src.config import HyperliquidConfig
from src.l1_collector.types import AssetCtx, L2BookLevel, L2BookSnapshot
from src.logging_setup import get_logger

log = get_logger("l1.rest")


class HLRestClient:
    """非同期 REST クライアント."""

    def __init__(self, cfg: HyperliquidConfig) -> None:
        self.cfg = cfg
        self._client = httpx.AsyncClient(timeout=10.0)

    async def close(self) -> None:
        await self._client.aclose()

    async def _post(self, payload: dict[str, Any]) -> Any:
        for attempt in range(5):
            try:
                resp = await self._client.post(self.cfg.info_api_url, json=payload)
                resp.raise_for_status()
                return resp.json()
            except (TimeoutError, httpx.HTTPError) as exc:
                wait = min(2**attempt, 30)
                log.warning(
                    "rest.error",
                    attempt=attempt,
                    wait=wait,
                    error=str(exc),
                    payload_type=payload.get("type"),
                )
                await asyncio.sleep(wait)
        raise RuntimeError(f"REST request failed after retries: {payload}")

    async def fetch_meta_and_asset_ctxs(self) -> list[AssetCtx]:
        """metaAndAssetCtxs を取得して symbols 別の AssetCtx へ展開."""
        data = await self._post({"type": "metaAndAssetCtxs"})
        meta = data[0]
        ctxs = data[1]
        universe = meta.get("universe", [])
        poll_ts = datetime.now(UTC)
        out: list[AssetCtx] = []
        for asset, ctx in zip(universe, ctxs, strict=False):
            symbol = asset.get("name", "")
            if symbol not in self.cfg.symbols:
                continue
            funding = ctx.get("funding")
            mark = ctx.get("markPx")
            oracle = ctx.get("oraclePx")
            day_volume = ctx.get("dayNtlVlm")
            oi = ctx.get("openInterest")
            impact_pxs = ctx.get("impactPxs")
            out.append(
                AssetCtx(
                    symbol=symbol,
                    poll_ts=poll_ts,
                    mark_px=float(mark) if mark is not None else None,
                    oracle_px=float(oracle) if oracle is not None else None,
                    funding_rate=float(funding) if funding is not None else None,
                    open_interest=float(oi) if oi is not None else None,
                    day_volume=float(day_volume) if day_volume is not None else None,
                    impact_pxs=(
                        (float(impact_pxs[0]), float(impact_pxs[1]))
                        if impact_pxs and len(impact_pxs) == 2
                        else None
                    ),
                )
            )
        return out

    async def fetch_l2_snapshot(self, symbol: str) -> L2BookSnapshot | None:
        """gap recovery 用: REST で板スナップショットを取得."""
        data = await self._post({"type": "l2Book", "coin": symbol})
        if not data:
            return None
        levels = data.get("levels") or [[], []]
        bids = [L2BookLevel(**lv) for lv in levels[0][: self.cfg.l2book_levels]]
        asks = [L2BookLevel(**lv) for lv in levels[1][: self.cfg.l2book_levels]]
        ts_field = data.get("time")
        recv_ts = datetime.now(UTC)
        exchange_ts = datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC) if ts_field else recv_ts
        return L2BookSnapshot(
            symbol=symbol,
            exchange_ts=exchange_ts,
            recv_ts=recv_ts,
            bids=bids,
            asks=asks,
            sequence=None,
            is_recovery_snapshot=True,
        )

    async def stream_asset_ctxs(self) -> AsyncIterator[list[AssetCtx]]:
        """rest_poll_interval_sec ごとに AssetCtx 一括取得."""
        while True:
            try:
                yield await self.fetch_meta_and_asset_ctxs()
            except Exception as exc:
                log.error("rest.poll.error", error=str(exc))
            await asyncio.sleep(self.cfg.rest_poll_interval_sec)
