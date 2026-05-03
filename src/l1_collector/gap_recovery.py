"""Gap recovery: WS sequence 不整合や切断時の snapshot リフレッシュ.

GapEvent を受け取り REST で l2book snapshot を取得して L2BookSnapshot として返す.
"""

from __future__ import annotations

from src.l1_collector.rest_client import HLRestClient
from src.l1_collector.types import GapEvent, L2BookSnapshot
from src.logging_setup import get_logger

log = get_logger("l1.gap")


class GapRecovery:
    """GapEvent を受けて REST snapshot で復旧."""

    def __init__(self, rest_client: HLRestClient) -> None:
        self.rest = rest_client

    async def recover(self, gap: GapEvent) -> L2BookSnapshot | None:
        log.warning(
            "gap.detected",
            symbol=gap.symbol,
            reason=gap.reason,
            last_seq=gap.last_seq,
            new_seq=gap.new_seq,
        )
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
