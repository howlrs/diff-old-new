"""Atomic Parquet writer (Gemini指摘: temp + os.rename で破損防止).

パーティション構造:
    data/raw/{table}/date={YYYY-MM-DD}/hour={HH}/part-{ts}.parquet
"""

from __future__ import annotations

import os
import uuid
from collections.abc import Iterable
from datetime import UTC, datetime
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

from src.config import StorageConfig
from src.logging_setup import get_logger

log = get_logger("l1.storage")


def _ensure_dir(p: Path) -> None:
    p.mkdir(parents=True, exist_ok=True)


def _partition_path(
    root: Path,
    table: str,
    when: datetime,
) -> Path:
    if when.tzinfo is None:
        when = when.replace(tzinfo=UTC)
    when_utc = when.astimezone(UTC)
    return (
        root / table / f"date={when_utc.strftime('%Y-%m-%d')}" / f"hour={when_utc.strftime('%H')}"
    )


def write_parquet_atomic(
    rows: Iterable[dict],
    table: str,
    cfg: StorageConfig,
    *,
    when: datetime | None = None,
    use_curated: bool = False,
) -> Path | None:
    """rows を1ファイルとして atomically 書き込む.

    破損防止 (Gemini指摘):
        1. temp ファイルに書く
        2. fsync で disk flush
        3. os.rename (POSIX で atomic) で final ファイル名へ

    Args:
        use_curated: True なら curated_data_root へ. False なら raw_data_root.
    """
    rows_list = list(rows)
    if not rows_list:
        log.debug("write_parquet_atomic.empty", table=table)
        return None

    when = when or datetime.now(UTC)
    root = cfg.curated_data_root if use_curated else cfg.raw_data_root
    part_dir = _partition_path(root, table, when)
    _ensure_dir(part_dir)

    # ファイル名: 衝突回避用に UUID を含める
    fname_final = f"part-{when.strftime('%Y%m%dT%H%M%S')}-{uuid.uuid4().hex[:8]}.parquet"
    fpath_final = part_dir / fname_final
    fpath_tmp = part_dir / (fname_final + ".tmp")

    table_pa = pa.Table.from_pylist(rows_list)
    pq.write_table(
        table_pa,
        fpath_tmp,
        compression=cfg.parquet_compression,
        use_dictionary=True,
    )

    # fsync で disk へ flush してから rename
    # Gemini指摘 (Improvement): O_RDONLY の fsync は環境依存なので O_RDWR で開く.
    fd = os.open(fpath_tmp, os.O_RDWR)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

    os.rename(fpath_tmp, fpath_final)

    # ディレクトリの fsync で rename を persist (best effort).
    try:
        dir_fd = os.open(part_dir, os.O_RDONLY)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except OSError:
        pass

    log.info(
        "parquet.written",
        table=table,
        path=str(fpath_final),
        rows=len(rows_list),
    )
    return fpath_final
