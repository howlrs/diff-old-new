"""Audit-A: 内部整合性監査 (バッチレポート).

Gemini partner の DoD: OOM せず処理し,
- データ欠損時間帯
- WS 切断回数
- 価格ジャンプの閾値超過件数
を Markdown レポートに集約.

検査項目 (詳細):
1. recv_ts vs exchange_ts ドリフト分布 (clock skew / latency)
2. exchange_ts の単調性 (重複 / 後退)
3. WS 切断ギャップ (l2book で is_recovery_snapshot が立つ前の空白)
4. mid 連続性 (隣接バー間 jump > 1% の件数)
5. 同時刻 oracle (asset_ctxs) と mid (l2book) の差
6. l2book 健全性: bid >= ask クロス件数, n=0 比率
"""

from __future__ import annotations

import itertools
import math
from dataclasses import dataclass, field
from pathlib import Path

import duckdb
import polars as pl


@dataclass
class SymbolAudit:
    """1 symbol あたりの監査メトリクス."""

    symbol: str
    n_l2book: int
    n_trades: int
    n_asset_ctxs: int
    # latency
    recv_minus_exchange_median_ms: float
    recv_minus_exchange_p95_ms: float
    recv_minus_exchange_p99_ms: float
    # exchange_ts 単調性
    n_ts_duplicates: int
    n_ts_backward: int
    # ギャップ
    n_long_gaps_30s: int  # > 30s 連続無受信の回数
    n_recovery_snapshots: int
    # price jump
    n_mid_jumps_over_1pct: int
    n_mid_jumps_over_5pct: int
    # oracle vs mid
    median_oracle_minus_mid_bps: float
    p95_abs_oracle_minus_mid_bps: float
    # l2book 健全性
    n_book_crossed: int  # best_bid >= best_ask
    pct_levels_with_zero_n: float


@dataclass
class AuditReport:
    raw_root: Path
    period_start: str
    period_end: str
    by_symbol: dict[str, SymbolAudit] = field(default_factory=dict)
    issues: list[str] = field(default_factory=list)


# -- helpers ---------------------------------------------------------------


def _percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    sv = sorted(values)
    idx = int(p * (len(sv) - 1))
    return sv[idx]


def _audit_one_symbol(
    con: duckdb.DuckDBPyConnection,
    raw_root: Path,
    symbol: str,
) -> SymbolAudit:
    l2_glob = str(raw_root / "l2book" / "**" / "*.parquet")
    tr_glob = str(raw_root / "trades" / "**" / "*.parquet")
    ctx_glob = str(raw_root / "asset_ctxs" / "**" / "*.parquet")

    # 1) latency / 単調性 (l2book)
    lat_q = (
        f"SELECT epoch_ms(recv_ts) - epoch_ms(exchange_ts) AS lat_ms, exchange_ts "
        f"FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' "
        f"ORDER BY exchange_ts"
    )
    lat_df = con.execute(lat_q).pl()
    n_l2 = lat_df.height
    lat_vals = lat_df["lat_ms"].drop_nulls().to_list() if n_l2 > 0 else []
    lat_med = _percentile(lat_vals, 0.5)
    lat_p95 = _percentile(lat_vals, 0.95)
    lat_p99 = _percentile(lat_vals, 0.99)

    # 重複 / 後退
    n_dup = 0
    n_back = 0
    n_long_gaps = 0
    if n_l2 >= 2:
        ts_list = lat_df["exchange_ts"].to_list()
        for prev, cur in itertools.pairwise(ts_list):
            if cur is None or prev is None:
                continue
            delta = (cur - prev).total_seconds()
            if delta == 0:
                n_dup += 1
            elif delta < 0:
                n_back += 1
            elif delta > 30:
                n_long_gaps += 1

    # recovery snapshot
    rec_q = (
        f"SELECT count(*) FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' AND is_recovery_snapshot = true"
    )
    n_rec = int(con.execute(rec_q).fetchone()[0])

    # mid jumps
    jump_q = (
        f"SELECT exchange_ts, mid FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' ORDER BY exchange_ts"
    )
    jump_df = con.execute(jump_q).pl()
    n_jump_1 = 0
    n_jump_5 = 0
    if jump_df.height >= 2:
        mids = jump_df["mid"].to_list()
        for prev, cur in itertools.pairwise(mids):
            if not prev or not cur or prev <= 0:
                continue
            delta = abs(cur - prev) / prev
            if delta > 0.01:
                n_jump_1 += 1
            if delta > 0.05:
                n_jump_5 += 1

    # oracle vs mid
    omd_q = (
        f"WITH l2 AS (SELECT exchange_ts, mid FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}'), "
        f"ctx AS (SELECT poll_ts, oracle_px FROM read_parquet('{ctx_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}'), "
        f"j AS ("
        f"  SELECT l2.exchange_ts, l2.mid, "
        f"  (SELECT oracle_px FROM ctx WHERE ctx.poll_ts <= l2.exchange_ts "
        f"    ORDER BY ctx.poll_ts DESC LIMIT 1) AS oracle_px "
        f"  FROM l2"
        f") "
        f"SELECT exchange_ts, mid, oracle_px, "
        f"  CASE WHEN mid > 0 AND oracle_px IS NOT NULL "
        f"    THEN (oracle_px - mid)/mid * 10000.0 ELSE NULL END AS oracle_minus_mid_bps "
        f"FROM j"
    )
    try:
        omd_df = con.execute(omd_q).pl()
    except duckdb.Error:
        omd_df = pl.DataFrame()
    diffs = omd_df["oracle_minus_mid_bps"].drop_nulls().to_list() if not omd_df.is_empty() else []
    median_diff = _percentile(diffs, 0.5)
    abs_diffs = [abs(v) for v in diffs]
    p95_abs = _percentile(abs_diffs, 0.95)

    # 板 健全性
    cross_q = (
        f"SELECT count(*) FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}' AND best_bid >= best_ask AND best_ask IS NOT NULL "
        f"AND best_bid IS NOT NULL"
    )
    n_cross = int(con.execute(cross_q).fetchone()[0])

    # n=0 levels 比率: bid_ns / ask_ns に 0 が含まれる割合
    nzero_q = (
        f"SELECT "
        f"  count(*) FILTER (WHERE list_contains(bid_ns, 0) OR list_contains(ask_ns, 0)) AS n_zero, "
        f"  count(*) AS n_total "
        f"FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol = '{symbol}'"
    )
    try:
        nzero_row = con.execute(nzero_q).fetchone()
        pct_zero = (nzero_row[0] / nzero_row[1] * 100.0) if nzero_row[1] > 0 else 0.0
    except duckdb.Error:
        pct_zero = 0.0

    # trades / asset_ctxs counts
    n_tr = int(
        con.execute(
            f"SELECT count(*) FROM read_parquet('{tr_glob}', union_by_name=true) WHERE symbol = '{symbol}'"
        ).fetchone()[0]
    )
    n_ctx = int(
        con.execute(
            f"SELECT count(*) FROM read_parquet('{ctx_glob}', union_by_name=true) WHERE symbol = '{symbol}'"
        ).fetchone()[0]
    )

    return SymbolAudit(
        symbol=symbol,
        n_l2book=n_l2,
        n_trades=n_tr,
        n_asset_ctxs=n_ctx,
        recv_minus_exchange_median_ms=lat_med,
        recv_minus_exchange_p95_ms=lat_p95,
        recv_minus_exchange_p99_ms=lat_p99,
        n_ts_duplicates=n_dup,
        n_ts_backward=n_back,
        n_long_gaps_30s=n_long_gaps,
        n_recovery_snapshots=n_rec,
        n_mid_jumps_over_1pct=n_jump_1,
        n_mid_jumps_over_5pct=n_jump_5,
        median_oracle_minus_mid_bps=median_diff,
        p95_abs_oracle_minus_mid_bps=p95_abs,
        n_book_crossed=n_cross,
        pct_levels_with_zero_n=pct_zero,
    )


def run_audit(raw_root: Path) -> AuditReport:
    """raw_root の全 symbol について Audit-A を実行."""
    con = duckdb.connect(":memory:")
    l2_glob = str(raw_root / "l2book" / "**" / "*.parquet")

    # symbols
    syms_df = con.execute(
        f"SELECT DISTINCT symbol FROM read_parquet('{l2_glob}', union_by_name=true) "
        f"WHERE symbol IS NOT NULL ORDER BY symbol"
    ).pl()
    symbols = syms_df["symbol"].to_list() if syms_df.height > 0 else []

    # period
    period = con.execute(
        f"SELECT min(exchange_ts), max(exchange_ts) FROM read_parquet('{l2_glob}', union_by_name=true)"
    ).fetchone()
    period_start = str(period[0]) if period[0] else "n/a"
    period_end = str(period[1]) if period[1] else "n/a"

    by_symbol: dict[str, SymbolAudit] = {}
    for sym in symbols:
        try:
            by_symbol[sym] = _audit_one_symbol(con, raw_root, sym)
        except Exception:
            by_symbol[sym] = SymbolAudit(
                symbol=sym,
                n_l2book=0,
                n_trades=0,
                n_asset_ctxs=0,
                recv_minus_exchange_median_ms=math.nan,
                recv_minus_exchange_p95_ms=math.nan,
                recv_minus_exchange_p99_ms=math.nan,
                n_ts_duplicates=-1,
                n_ts_backward=-1,
                n_long_gaps_30s=-1,
                n_recovery_snapshots=-1,
                n_mid_jumps_over_1pct=-1,
                n_mid_jumps_over_5pct=-1,
                median_oracle_minus_mid_bps=math.nan,
                p95_abs_oracle_minus_mid_bps=math.nan,
                n_book_crossed=-1,
                pct_levels_with_zero_n=math.nan,
            )
            continue

    return AuditReport(
        raw_root=raw_root,
        period_start=period_start,
        period_end=period_end,
        by_symbol=by_symbol,
    )


def render_markdown(report: AuditReport) -> str:
    lines: list[str] = []
    lines.append("# Audit-A: internal consistency report")
    lines.append("")
    lines.append(f"raw_root: `{report.raw_root}`")
    lines.append(f"period: {report.period_start} → {report.period_end}")
    lines.append(f"symbols: {len(report.by_symbol)}")
    lines.append("")
    if not report.by_symbol:
        lines.append("> ⚠ no l2book data found")
        return "\n".join(lines)

    lines.append("## 受信レイテンシ (recv_ts - exchange_ts, ms)")
    lines.append("")
    lines.append("| symbol | n_l2 | median | p95 | p99 |")
    lines.append("|---|---|---|---|---|")
    for sym, a in report.by_symbol.items():
        lines.append(
            f"| {sym} | {a.n_l2book} | "
            f"{a.recv_minus_exchange_median_ms:.0f} | "
            f"{a.recv_minus_exchange_p95_ms:.0f} | "
            f"{a.recv_minus_exchange_p99_ms:.0f} |"
        )
    lines.append("")

    lines.append("## 単調性 / 切断 / リカバリー")
    lines.append("")
    lines.append("| symbol | dup | backward | long_gap_30s | recovery_snap |")
    lines.append("|---|---|---|---|---|")
    for sym, a in report.by_symbol.items():
        lines.append(
            f"| {sym} | {a.n_ts_duplicates} | {a.n_ts_backward} | "
            f"{a.n_long_gaps_30s} | {a.n_recovery_snapshots} |"
        )
    lines.append("")

    lines.append("## 価格ジャンプ (隣接バー間 mid 変化率)")
    lines.append("")
    lines.append("| symbol | >1% | >5% |")
    lines.append("|---|---|---|")
    for sym, a in report.by_symbol.items():
        lines.append(f"| {sym} | {a.n_mid_jumps_over_1pct} | {a.n_mid_jumps_over_5pct} |")
    lines.append("")

    lines.append("## Oracle (asset_ctxs.oracle_px) vs Mid (l2book.mid)")
    lines.append("")
    lines.append("| symbol | median diff (bps) | p95 abs diff (bps) |")
    lines.append("|---|---|---|")
    for sym, a in report.by_symbol.items():
        lines.append(
            f"| {sym} | {a.median_oracle_minus_mid_bps:+.1f} | {a.p95_abs_oracle_minus_mid_bps:.1f} |"
        )
    lines.append("")

    lines.append("## 板 健全性")
    lines.append("")
    lines.append("| symbol | crossed bid>=ask | n=0 levels (%) |")
    lines.append("|---|---|---|")
    for sym, a in report.by_symbol.items():
        lines.append(f"| {sym} | {a.n_book_crossed} | {a.pct_levels_with_zero_n:.2f}% |")
    lines.append("")

    lines.append("## 解釈ガイド")
    lines.append("")
    lines.append(
        "- **latency**: WS 経由なら medianは数百 ms 以下が健全. p99 が秒オーダー超なら NTP / network 問題"
    )
    lines.append("- **dup / backward**: 0 が理想. backward > 0 はサーバー側 bug / clock skew")
    lines.append("- **long_gap_30s**: 30 秒以上無受信の連続 → WS切断未検出 or recovery 漏れの疑い")
    lines.append(
        "- **mid jump >1%**: closure 中の oracle ワープを除き発生稀. 多発なら data corruption 疑い"
    )
    lines.append("- **oracle vs mid**: closure 中は乖離大きい (HL内部 EMA 由来) のが正常")
    lines.append("- **板 crossed**: 0 が必須. >0 は重大 (受信順序逆転 / parsing bug)")

    return "\n".join(lines)
