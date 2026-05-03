"""L1 storage の atomic write と date partition."""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pyarrow.parquet as pq

from src.config import StorageConfig
from src.l1_collector.storage import write_parquet_atomic


def test_atomic_write_creates_partitioned_file(tmp_path: Path) -> None:
    cfg = StorageConfig(raw_data_root=tmp_path)
    when = datetime(2026, 5, 4, 12, 30, 0, tzinfo=UTC)
    rows = [{"a": 1, "b": "x"}, {"a": 2, "b": "y"}]
    out = write_parquet_atomic(rows, "test_table", cfg, when=when)
    assert out is not None
    assert out.exists()
    # partition path
    rel = out.relative_to(tmp_path)
    parts = rel.parts
    assert parts[0] == "test_table"
    assert parts[1] == "date=2026-05-04"
    assert parts[2] == "hour=12"

    # readable parquet (partition columns may be auto-added by pyarrow)
    table = pq.read_table(out)
    assert table.num_rows == 2
    assert {"a", "b"}.issubset(set(table.column_names))


def test_atomic_write_no_tmp_remains(tmp_path: Path) -> None:
    cfg = StorageConfig(raw_data_root=tmp_path)
    rows = [{"a": 1}]
    out = write_parquet_atomic(rows, "tt", cfg)
    assert out is not None
    # .tmp が残っていない
    tmp_files = list(tmp_path.rglob("*.tmp"))
    assert tmp_files == []


def test_empty_rows_returns_none(tmp_path: Path) -> None:
    cfg = StorageConfig(raw_data_root=tmp_path)
    out = write_parquet_atomic([], "tt", cfg)
    assert out is None
