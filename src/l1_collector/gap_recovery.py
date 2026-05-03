"""Gap recovery: WS sequence 不整合や切断時の snapshot リフレッシュ.

GapEvent を受け取り REST で l2book snapshot を取得して L2BookSnapshot として返す.

Gemini指摘 (Improvement) 反映:
    複数銘柄で同時に GapEvent が発生すると REST API rate limit に抵触する恐れ.
    Semaphore で同時実行数を制限.
"""

from __future__ import annotations

import asyncio

from src.l1_collector.rest_client import HLRestClient
from src.l1_collector.types import GapEvent, L2BookSnapshot
from src.logging_setup import get_logger

log = get_logger("l1.gap")

DEFAULT_MAX_CONCURRENT = 3


class GapRecovery:
    """GapEvent を受けて REST snapshot で復旧."""

    def __init__(
        self,
        rest_client: HLRestClient,
        max_concurrent: int = DEFAULT_MAX_CONCURRENT,
    ) -> None:
        self.rest = rest_client
        self._sem = asyncio.Semaphore(max_concurrent)

    async def recover(self, gap: GapEvent) -> L2BookSnapshot | None:
        log.warning(
            "gap.detected",
            symbol=gap.symbol,
            reason=gap.reason,
            last_seq=gap.last_seq,
            new_seq=gap.new_seq,
        )
        async with self._sem:
            snapshot = await self.rest.fetch_l2_snapshot(gap.symbol)
        if snapshot is not None:
            log.info(
                "gap.recovered",
                symbol=gap.symbol,
                snapshot_ts=snapshot.exchange_ts.isoformat(),
            )
        else:
            log.error("gap.recovery_failed", symbol=gap.symbol)
        return snapshot
