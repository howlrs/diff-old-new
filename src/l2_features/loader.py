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
    *,
    is_curated: bool = False,
) -> pl.DataFrame:
    """Parquet partition を読み込んで Polars DataFrame で返す.

    Gemini指摘 (Improvement) 反映: is_curated で raw/curated を切り替える.
    呼び出し側で StorageConfig を model_copy するハックを廃止.
    """
    root = cfg.curated_data_root if is_curated else cfg.raw_data_root
    glob = _partition_glob(root, table, day)
    log.debug("loader.read", table=table, glob=glob, is_curated=is_curated)
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


def load_features(cfg: StorageConfig, day: date_t | None = None) -> pl.DataFrame:
    """L2 出力 (curated) を読む."""
    return load_table("features", cfg, day, is_curated=True)
