"""L1 runner の graceful shutdown と最終 flush の保証テスト (Issue #30)."""

from __future__ import annotations

import asyncio
from collections import deque
from datetime import UTC, datetime
from pathlib import Path

import pyarrow.parquet as pq
import pytest

from src.config import AppConfig
from src.l1_collector.runner import L1Runner


@pytest.mark.asyncio
async def test_request_stop_flushes_remaining_buffer(tmp_path: Path) -> None:
    """request_stop() で run() が抜け, バッファに残ったデータが Parquet に flush される."""
    cfg = AppConfig()
    cfg.storage.raw_data_root = tmp_path / "raw"
    cfg.storage.curated_data_root = tmp_path / "curated"
    cfg.logging.heartbeat_interval_sec = 3600

    runner = L1Runner(cfg)

    # WS / REST タスクを stub に差し替えて, バッファに事前データを入れておく
    async def _noop() -> None:
        await asyncio.sleep(60)

    runner._run_ws = _noop  # type: ignore[method-assign]
    runner._run_rest_poll = _noop  # type: ignore[method-assign]
    runner._run_heartbeat = _noop  # type: ignore[method-assign]

    runner._l2_buf = deque(
        [
            {
                "symbol": "BTC",
                "exchange_ts": datetime.now(UTC),
                "recv_ts": datetime.now(UTC),
                "is_recovery_snapshot": False,
                "best_bid": 100.0,
                "best_ask": 101.0,
                "mid": 100.5,
                "bid_pxs": [100.0],
                "bid_szs": [1.0],
                "bid_ns": [1],
                "ask_pxs": [101.0],
                "ask_szs": [1.0],
                "ask_ns": [1],
            }
        ]
    )

    # 別タスクで run, 0.2 秒後に stop
    runner_task = asyncio.create_task(runner.run())
    await asyncio.sleep(0.2)
    runner.request_stop()
    await asyncio.wait_for(runner_task, timeout=3)

    # raw/l2book/date=.../hour=.../part-*.parquet が書き込まれていること
    written = list(tmp_path.rglob("l2book/**/part-*.parquet"))
    assert len(written) == 1, f"Expected 1 parquet written, found: {written}"
    table = pq.read_table(written[0])
    assert table.num_rows == 1
    assert "BTC" in [str(v) for v in table.column("symbol").to_pylist()]
