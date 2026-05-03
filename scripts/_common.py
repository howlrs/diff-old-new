"""KPI / 分析スクリプトの共通ユーティリティ.

Gemini指摘 (PR #32 review) の防御的実装をここに集約.
"""

from __future__ import annotations

import glob as glob_mod
from pathlib import Path

import duckdb
import polars as pl


def safe_read_parquet_glob(glob_str: str) -> pl.DataFrame:
    """glob にマッチするファイルが無い場合に空 DataFrame を返す.

    Gemini指摘 (Bug): DuckDB の read_parquet は glob 不一致で例外を投げるので
    Python 側で事前にファイル数を確認する.
    """
    matches = glob_mod.glob(glob_str, recursive=True)
    if not matches:
        return pl.DataFrame()
    con = duckdb.connect(":memory:")
    arrow_tbl = con.execute(f"SELECT * FROM read_parquet('{glob_str}', union_by_name=true)").arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()
    return df


def expect_parquet_dir(root: Path, table: str) -> str:
    """パーティション root から glob パターンを生成."""
    return str(root / table / "**" / "*.parquet")
