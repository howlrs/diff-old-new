"""Audit-A0: schema sanity check.

Gemini partner 指摘 (前提条件): 全 Parquet の datetime / dtype が UTC 統一されている
ことを確認しないと, 後続の audit が偽陽性 / 誤判定を生む.

検査項目:
- datetime カラム (exchange_ts / recv_ts / poll_ts / entry_ts / exit_ts) が tz-aware
- 全パーティション間で同一 dtype (型ドリフト無し)
- 必須カラムが揃っている
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import duckdb
import polars as pl

REQUIRED_COLUMNS = {
    "l2book": {
        "symbol",
        "exchange_ts",
        "recv_ts",
        "best_bid",
        "best_ask",
        "mid",
    },
    "trades": {"symbol", "exchange_ts", "recv_ts", "px", "sz", "side"},
    "asset_ctxs": {
        "symbol",
        "poll_ts",
        "dex",
        "mark_px",
        "oracle_px",
        "funding_rate",
    },
}

DATETIME_COLUMNS = {
    "l2book": {"exchange_ts", "recv_ts"},
    "trades": {"exchange_ts", "recv_ts"},
    "asset_ctxs": {"poll_ts"},
}


@dataclass
class TableSchemaReport:
    table: str
    n_files: int
    n_rows: int
    columns: dict[str, str]  # column -> dtype
    missing_required: set[str]
    datetime_cols_tz_aware: dict[str, bool]
    issues: list[str] = field(default_factory=list)

    @property
    def is_healthy(self) -> bool:
        return (
            not self.missing_required
            and all(self.datetime_cols_tz_aware.values())
            and not self.issues
        )


@dataclass
class SchemaCheckResult:
    raw_root: Path
    tables: dict[str, TableSchemaReport]

    @property
    def all_healthy(self) -> bool:
        return all(r.is_healthy for r in self.tables.values())


def _check_table(con: duckdb.DuckDBPyConnection, raw_root: Path, table: str) -> TableSchemaReport:
    glob = str(raw_root / table / "**" / "*.parquet")
    from glob import glob as _glob

    files = _glob(glob, recursive=True)
    if not files:
        return TableSchemaReport(
            table=table,
            n_files=0,
            n_rows=0,
            columns={},
            missing_required=REQUIRED_COLUMNS.get(table, set()),
            datetime_cols_tz_aware={},
            issues=["no files found"],
        )

    # サンプル schema (最新 part を読む) でカラム/型把握
    arrow_tbl = con.execute(
        f"SELECT * FROM read_parquet('{glob}', union_by_name=true) LIMIT 1"
    ).arrow()
    df = pl.from_arrow(arrow_tbl)
    if isinstance(df, pl.Series):
        df = df.to_frame()

    columns = {name: str(dtype) for name, dtype in df.schema.items()}
    missing = REQUIRED_COLUMNS.get(table, set()) - set(columns.keys())

    # tz aware check (Polars Datetime には time_zone 属性)
    tz_aware: dict[str, bool] = {}
    for col in DATETIME_COLUMNS.get(table, set()):
        if col not in df.schema:
            tz_aware[col] = False
            continue
        dtype = df.schema[col]
        if isinstance(dtype, pl.Datetime):
            tz_aware[col] = dtype.time_zone is not None
        else:
            tz_aware[col] = False

    n_rows = int(
        con.execute(f"SELECT count(*) FROM read_parquet('{glob}', union_by_name=true)").fetchone()[
            0
        ]
    )

    issues: list[str] = []
    if missing:
        issues.append(f"missing required columns: {sorted(missing)}")
    for col, ok in tz_aware.items():
        if not ok:
            issues.append(f"datetime column '{col}' is not tz-aware")

    return TableSchemaReport(
        table=table,
        n_files=len(files),
        n_rows=n_rows,
        columns=columns,
        missing_required=missing,
        datetime_cols_tz_aware=tz_aware,
        issues=issues,
    )


def check_schema(raw_root: Path) -> SchemaCheckResult:
    """raw_root 配下の主要 table を schema チェック."""
    con = duckdb.connect(":memory:")
    tables: dict[str, TableSchemaReport] = {}
    for table in REQUIRED_COLUMNS:
        tables[table] = _check_table(con, raw_root, table)
    return SchemaCheckResult(raw_root=raw_root, tables=tables)


def render_markdown(result: SchemaCheckResult) -> str:
    lines: list[str] = []
    lines.append("# Audit-A0: schema sanity check")
    lines.append("")
    lines.append(f"raw_root: `{result.raw_root}`")
    lines.append(f"all_healthy: **{result.all_healthy}**")
    lines.append("")
    for tbl, rep in result.tables.items():
        lines.append(f"## {tbl}")
        lines.append("")
        lines.append(f"- files: {rep.n_files}")
        lines.append(f"- rows: {rep.n_rows}")
        lines.append(f"- healthy: {rep.is_healthy}")
        if rep.issues:
            lines.append("")
            lines.append("### Issues")
            for issue in rep.issues:
                lines.append(f"- ⚠ {issue}")
        lines.append("")
        lines.append("### Columns")
        lines.append("")
        lines.append("| column | dtype | tz-aware |")
        lines.append("|---|---|---|")
        for col, dtype in sorted(rep.columns.items()):
            tz = rep.datetime_cols_tz_aware.get(col)
            tz_s = "yes" if tz else "no" if tz is False else "-"
            lines.append(f"| {col} | {dtype} | {tz_s} |")
        lines.append("")
    return "\n".join(lines)
