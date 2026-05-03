"""GUI から見たデータアクセス層 (DuckDB ラッパー).

src/l2_features/loader.py が L2 pipeline 内で使っているのと同じ DuckDB を,
GUI 用に共通化. notebook 1個に対して 1 connection.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import duckdb
import polars as pl

from src.config import StorageConfig


@dataclass
class GuiDataSource:
    """marimo notebook が抱える DuckDB connection と各 glob.

    使い方:
        ds = GuiDataSource.from_config(cfg.storage)
        df = ds.load_backtest_trades("H1_closure_mean_rev")
    """

    raw_root: Path
    curated_root: Path
    kpi_dir: Path
    con: duckdb.DuckDBPyConnection

    @classmethod
    def from_config(cls, storage: StorageConfig, kpi_dir: Path | None = None) -> GuiDataSource:
        return cls(
            raw_root=storage.raw_data_root,
            curated_root=storage.curated_data_root,
            kpi_dir=kpi_dir or Path("docs/kpi"),
            con=duckdb.connect(":memory:"),
        )

    # -------- raw --------

    def _glob_raw(self, table: str) -> str:
        return str(self.raw_root / table / "**" / "*.parquet")

    def _glob_curated(self, table: str) -> str:
        return str(self.curated_root / table / "**" / "*.parquet")

    def _safe_query(self, glob: str, where: str = "") -> pl.DataFrame:
        from glob import glob as _glob

        if not _glob(glob, recursive=True):
            return pl.DataFrame()
        sql = f"SELECT * FROM read_parquet('{glob}', union_by_name=true)"
        if where:
            sql += f" WHERE {where}"
        arrow_tbl = self.con.execute(sql).arrow()
        df = pl.from_arrow(arrow_tbl)
        if isinstance(df, pl.Series):
            df = df.to_frame()
        return df

    def list_symbols(self) -> list[str]:
        df = self._safe_query(self._glob_raw("l2book"))
        if df.is_empty() or "symbol" not in df.columns:
            return []
        return sorted(df["symbol"].unique().drop_nulls().to_list())

    def load_l2book(self, symbol: str | None = None) -> pl.DataFrame:
        where = f"symbol = '{symbol}'" if symbol else ""
        return self._safe_query(self._glob_raw("l2book"), where)

    def load_trades(self, symbol: str | None = None) -> pl.DataFrame:
        where = f"symbol = '{symbol}'" if symbol else ""
        return self._safe_query(self._glob_raw("trades"), where)

    def load_asset_ctxs(self, symbol: str | None = None) -> pl.DataFrame:
        where = f"symbol = '{symbol}'" if symbol else ""
        return self._safe_query(self._glob_raw("asset_ctxs"), where)

    # -------- curated --------

    def load_features(self, symbol: str | None = None) -> pl.DataFrame:
        where = f"symbol = '{symbol}'" if symbol else ""
        return self._safe_query(self._glob_curated("features"), where)

    # -------- backtest results --------

    def list_backtest_strategies(self) -> list[str]:
        bt_root = self.curated_root / "backtest_results"
        if not bt_root.exists():
            return []
        return sorted(p.name for p in bt_root.iterdir() if p.is_dir())

    def load_backtest_trades(
        self,
        strategy: str,
        symbol: str | None = None,
        run_id: str | None = None,
    ) -> pl.DataFrame:
        glob = self._glob_curated(f"backtest_results/{strategy}")
        wheres: list[str] = []
        if symbol:
            wheres.append(f"symbol = '{symbol}'")
        if run_id:
            wheres.append(f"run_id = '{run_id}'")
        where = " AND ".join(wheres)
        return self._safe_query(glob, where)

    # -------- KPI --------

    def load_kpi_json(self, name: str) -> dict:
        import json

        p = self.kpi_dir / f"{name}.json"
        if not p.exists():
            return {}
        return json.loads(p.read_text(encoding="utf-8"))

    def load_kpi_md(self, name: str) -> str:
        p = self.kpi_dir / f"{name}.md"
        return p.read_text(encoding="utf-8") if p.exists() else ""
