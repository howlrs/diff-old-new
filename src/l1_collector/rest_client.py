"""Hyperliquid REST poller (meta / metaAndAssetCtxs / l2Book snapshot).

dry-run (2026-05-04) で判明した HIP-3 仕様:
- 米株 perp は dex="xyz" を指定して metaAndAssetCtxs を叩く
- core perp (BTC/ETH 等) は dex="" (省略可)
- 本クライアントは core/xyz の両方を1サイクルで polling し AssetCtx を produce

Gemini指摘 (Bug 2) 反映: レスポンスの形式を防御的に検証.
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


def _f(value: Any) -> float | None:
    if value is None or value == "":
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


class HLRestClient:
    """非同期 REST クライアント (HIP-3 dex 対応)."""

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
            except (TimeoutError, httpx.TimeoutException, httpx.HTTPError) as exc:
                wait = min(2**attempt, 30)
                log.warning(
                    "rest.error",
                    attempt=attempt,
                    wait=wait,
                    error=str(exc),
                    payload_type=payload.get("type"),
                    payload_dex=payload.get("dex"),
                )
                await asyncio.sleep(wait)
        raise RuntimeError(f"REST request failed after retries: {payload}")

    async def fetch_meta_and_asset_ctxs(
        self,
        dex: str,
        target_symbols: list[str],
    ) -> list[AssetCtx]:
        """指定 dex の metaAndAssetCtxs を取得し AssetCtx へ展開.

        Bug 2 反映: 空配列・形式違いを防御的にチェック.
        """
        payload: dict[str, Any] = {"type": "metaAndAssetCtxs"}
        if dex:
            payload["dex"] = dex
        data = await self._post(payload)
        if not isinstance(data, list) or len(data) < 2:
            log.warning(
                "rest.unexpected_response",
                payload_type="metaAndAssetCtxs",
                dex=dex,
            )
            return []
        meta = data[0] if isinstance(data[0], dict) else {}
        ctxs = data[1] if isinstance(data[1], list) else []
        universe = meta.get("universe", [])
        poll_ts = datetime.now(UTC)
        out: list[AssetCtx] = []
        for asset, ctx in zip(universe, ctxs, strict=False):
            symbol = asset.get("name", "")
            if symbol not in target_symbols:
                continue
            if not isinstance(ctx, dict):
                continue
            impact_pxs = ctx.get("impactPxs")
            ib = ia = None
            if isinstance(impact_pxs, list) and len(impact_pxs) >= 2:
                ib = _f(impact_pxs[0])
                ia = _f(impact_pxs[1])
            out.append(
                AssetCtx(
                    symbol=symbol,
                    poll_ts=poll_ts,
                    dex=dex,
                    mark_px=_f(ctx.get("markPx")),
                    oracle_px=_f(ctx.get("oraclePx")),
                    mid_px=_f(ctx.get("midPx")),
                    funding_rate=_f(ctx.get("funding")),
                    premium=_f(ctx.get("premium")),
                    open_interest=_f(ctx.get("openInterest")),
                    day_volume=_f(ctx.get("dayNtlVlm")),
                    day_base_volume=_f(ctx.get("dayBaseVlm")),
                    prev_day_px=_f(ctx.get("prevDayPx")),
                    impact_bid_px=ib,
                    impact_ask_px=ia,
                )
            )
        return out

    async def fetch_all_asset_ctxs(self) -> list[AssetCtx]:
        """core (dex="") と xyz の両方を一度に取得."""
        results: list[AssetCtx] = []
        if self.cfg.core_symbols:
            results.extend(await self.fetch_meta_and_asset_ctxs("", self.cfg.core_symbols))
        if self.cfg.xyz_symbols:
            results.extend(
                await self.fetch_meta_and_asset_ctxs(self.cfg.xyz_dex_name, self.cfg.xyz_symbols)
            )
        return results

    async def fetch_l2_snapshot(self, symbol: str) -> L2BookSnapshot | None:
        """gap recovery 用の REST l2Book snapshot."""
        data = await self._post({"type": "l2Book", "coin": symbol})
        if not isinstance(data, dict):
            return None
        levels = data.get("levels") or [[], []]
        bids_raw = levels[0][: self.cfg.l2book_levels] if len(levels) > 0 else []
        asks_raw = levels[1][: self.cfg.l2book_levels] if len(levels) > 1 else []
        bids = [L2BookLevel(**lv) for lv in bids_raw]
        asks = [L2BookLevel(**lv) for lv in asks_raw]
        ts_field = data.get("time")
        recv_ts = datetime.now(UTC)
        exchange_ts = (
            datetime.fromtimestamp(int(ts_field) / 1000, tz=UTC)
            if ts_field is not None
            else recv_ts
        )
        return L2BookSnapshot(
            symbol=symbol,
            exchange_ts=exchange_ts,
            recv_ts=recv_ts,
            bids=bids,
            asks=asks,
            is_recovery_snapshot=True,
        )

    async def stream_asset_ctxs(self) -> AsyncIterator[list[AssetCtx]]:
        """rest_poll_interval_sec ごとに core+xyz の AssetCtx を返す."""
        while True:
            try:
                yield await self.fetch_all_asset_ctxs()
            except Exception as exc:
                log.error("rest.poll.error", error=str(exc))
            await asyncio.sleep(self.cfg.rest_poll_interval_sec)
