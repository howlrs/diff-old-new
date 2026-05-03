"""data/raw → DuckDB → Polars DataFrame ローダ.

date-partitioned Parquet を DuckDB 経由で効率良く読み込む.
"""

from __future__ import annotations

from datetime import date as date_t
from pathlib import Path

import duckdb
import polars as pl

from src.config import StorageConfig
from src.logging_setup import get_logger

log = get_logger("l2.loader")


def _partition_glob(root: Path, table: str, day: date_t | None) -> str:
    if day is not None:
        return str(root / table / f"date={day.isoformat()}" / "**" / "*.parquet")
    return str(root / table / "**" / "*.parquet")


def load_table(
    table: str,
    cfg: StorageConfig,
    day: date_t | None = None,
) -> pl.DataFrame:
    """Parquet partition を読み込んで Polars DataFrame で返す."""
    glob = _partition_glob(cfg.raw_data_root, table, day)
    log.debug("loader.read", table=table, glob=glob)
    con = duckdb.connect(":memory:")
    arrow_tbl = con.execute(f"SELECT * FROM read_parquet('{glob}', union_by_name=true)").arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()
    return df.sort("exchange_ts" if "exchange_ts" in df.columns else df.columns[0])


def load_l2book(cfg: StorageConfig, day: date_t | None = None) -> pl.DataFrame:
    return load_table("l2book", cfg, day)


def load_trades(cfg: StorageConfig, day: date_t | None = None) -> pl.DataFrame:
    return load_table("trades", cfg, day)


def load_asset_ctxs(cfg: StorageConfig, day: date_t | None = None) -> pl.DataFrame:
    return load_table("asset_ctxs", cfg, day)
