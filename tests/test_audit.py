"""Audit pipeline (A0/A/B/D) のテスト."""

from __future__ import annotations

from pathlib import Path

import polars as pl

from src.audit.external_benchmark import (
    ExternalAuditReport,
    ExternalBenchmark,
    align_minute_bars,
    weighted_median_close,
)
from src.audit.internal_consistency import AuditReport, SymbolAudit
from src.audit.quality_score import (
    _score_external,
    _score_internal,
    compute_quality_score,
)
from src.audit.schema_check import check_schema


def test_schema_check_empty_dirs(tmp_path: Path) -> None:
    result = check_schema(tmp_path)
    assert not result.all_healthy
    for _tbl, rep in result.tables.items():
        assert rep.n_files == 0
        assert "no files found" in rep.issues


def _healthy_audit() -> SymbolAudit:
    return SymbolAudit(
        symbol="BTC",
        n_l2book=10000,
        n_trades=5000,
        n_asset_ctxs=100,
        recv_minus_exchange_median_ms=300,
        recv_minus_exchange_p95_ms=500,
        recv_minus_exchange_p99_ms=800,
        n_ts_duplicates=0,
        n_ts_backward=0,
        n_long_gaps_30s=0,
        n_recovery_snapshots=0,
        n_mid_jumps_over_1pct=0,
        n_mid_jumps_over_5pct=0,
        median_oracle_minus_mid_bps=0.0,
        p95_abs_oracle_minus_mid_bps=5.0,
        n_book_crossed=0,
        pct_levels_with_zero_n=0.0,
    )


def test_score_internal_healthy_full_score() -> None:
    score, deductions = _score_internal(_healthy_audit())
    assert score == 70.0
    assert deductions == []


def test_score_internal_with_clock_skew() -> None:
    audit = _healthy_audit()
    audit.n_ts_backward = 5
    score, deductions = _score_internal(audit)
    assert score < 70.0
    assert any(rule == "ts_backward" for rule, _, _ in deductions)


def test_score_internal_with_crossed_book() -> None:
    audit = _healthy_audit()
    audit.n_book_crossed = 3
    score, deductions = _score_internal(audit)
    assert score < 70.0
    assert any(rule == "book_crossed" for rule, _, _ in deductions)


def test_score_external_full_score() -> None:
    bench = ExternalBenchmark(
        symbol="BTC",
        benchmark_name="cex",
        n_aligned=100,
        correlation=0.99,
        median_diff_bps=0.5,
        p95_abs_diff_bps=3.0,
        max_abs_diff_bps=10.0,
        period_start="-",
        period_end="-",
    )
    score, deductions = _score_external(bench)
    assert score == 30.0
    assert deductions == []


def test_score_external_low_corr() -> None:
    bench = ExternalBenchmark(
        symbol="BTC",
        benchmark_name="cex",
        n_aligned=100,
        correlation=0.80,
        median_diff_bps=0.5,
        p95_abs_diff_bps=3.0,
        max_abs_diff_bps=10.0,
        period_start="-",
        period_end="-",
    )
    score, deductions = _score_external(bench)
    assert score < 30.0
    assert any(rule == "external_corr_low" for rule, _, _ in deductions)


def test_score_external_market_closed_excuse() -> None:
    score_no, _ = _score_external(None, market_closed_excuse=False)
    score_closed, ded_closed = _score_external(None, market_closed_excuse=True)
    assert score_closed > score_no
    assert any(rule == "external_market_closed" for rule, _, _ in ded_closed)


def test_align_minute_bars_inner_join() -> None:
    import datetime as dt

    base = dt.datetime(2026, 5, 4, 12, tzinfo=dt.UTC)
    hl = pl.DataFrame(
        {
            "ts": [base + dt.timedelta(minutes=i) for i in range(5)],
            "oracle_px": [100.0, 100.5, 101.0, 101.5, 102.0],
        }
    )
    bench = pl.DataFrame(
        {
            "ts": [base + dt.timedelta(minutes=i, seconds=10) for i in range(5)],
            "close": [100.0, 100.4, 101.1, 101.5, 102.0],
        }
    )
    aligned = align_minute_bars(hl, bench)
    # 1 分 floor で 5 件マッチ
    assert aligned.height == 5
    assert "diff_bps" in aligned.columns


def test_weighted_median_close_with_3_sources() -> None:
    import datetime as dt

    base = dt.datetime(2026, 5, 4, 12, tzinfo=dt.UTC)
    sources = {
        "binance": pl.DataFrame({"ts": [base], "close": [100.0]}),
        "bybit": pl.DataFrame({"ts": [base], "close": [101.0]}),
        "okx": pl.DataFrame({"ts": [base], "close": [99.0]}),
    }
    weights = {"binance": 3.0, "bybit": 2.0, "okx": 2.0}
    out = weighted_median_close(sources, weights)
    assert out.height == 1
    # weighted median: 3 * 100 = 99, 99, 100, 100, 100, 101, 101 → median = 100
    assert out["close"][0] == 100.0


def test_compute_quality_score_btc_perfect() -> None:
    internal = AuditReport(
        raw_root=Path("."),
        period_start="-",
        period_end="-",
        by_symbol={"BTC": _healthy_audit()},
    )
    external = ExternalAuditReport(
        benchmarks=[
            ExternalBenchmark(
                symbol="BTC",
                benchmark_name="cex",
                n_aligned=100,
                correlation=0.99,
                median_diff_bps=0.0,
                p95_abs_diff_bps=2.0,
                max_abs_diff_bps=5.0,
                period_start="-",
                period_end="-",
            )
        ]
    )
    cards = compute_quality_score(internal, external)
    assert "BTC" in cards
    assert cards["BTC"].score == 100.0
    assert not cards["BTC"].is_warning
