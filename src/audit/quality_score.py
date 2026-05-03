"""Audit-D: data quality score 算出.

Audit-A (内部整合性) と Audit-B (外部突合) のメトリクスを 0-100 score に集約.
GUI dashboard で「現在のデータの信頼度」を一目で示す.

スコア構成 (各 weight は config 化可能):
- internal latency (median <500ms = 満点)
- internal consistency (dup=0, backward=0, crossed=0)
- gap recovery (long_gap_30s が低い)
- mid jump (>1% が低い)
- external corr (BTC vs CEX 中央値の相関 >0.95)
- external bias (median diff bps が小さい)

満点 100 / 警告閾値 95.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime

from src.audit.external_benchmark import ExternalAuditReport, ExternalBenchmark
from src.audit.internal_consistency import AuditReport, SymbolAudit
from src.l2_features.regime import Regime, classify_regime


@dataclass
class QualityScoreCard:
    """1 銘柄の品質スコアカード."""

    symbol: str
    score: float  # 0-100
    deductions: list[tuple[str, float, str]]  # (rule_id, points_deducted, message)

    @property
    def is_warning(self) -> bool:
        return self.score < 95.0

    def summary_line(self) -> str:
        flag = "⚠" if self.is_warning else "✓"
        return f"{flag} {self.symbol}: {self.score:.1f}/100"


def _score_internal(audit: SymbolAudit) -> tuple[float, list[tuple[str, float, str]]]:
    """内部整合性 0-70 のうちでスコアリング."""
    score = 70.0
    deductions: list[tuple[str, float, str]] = []

    # latency: median > 1000ms で減点
    if audit.recv_minus_exchange_median_ms > 1000:
        d = min(10.0, (audit.recv_minus_exchange_median_ms - 1000) / 100)
        score -= d
        deductions.append(
            (
                "latency_median_high",
                d,
                f"median latency {audit.recv_minus_exchange_median_ms:.0f}ms > 1000ms",
            )
        )

    # p99 latency > 5000ms で減点
    if audit.recv_minus_exchange_p99_ms > 5000:
        d = min(10.0, (audit.recv_minus_exchange_p99_ms - 5000) / 1000)
        score -= d
        deductions.append(
            (
                "latency_p99_high",
                d,
                f"p99 latency {audit.recv_minus_exchange_p99_ms:.0f}ms > 5000ms",
            )
        )

    # backward (clock skew)
    if audit.n_ts_backward > 0:
        d = min(15.0, audit.n_ts_backward * 3.0)
        score -= d
        deductions.append(("ts_backward", d, f"{audit.n_ts_backward} backward timestamps"))

    # crossed book
    if audit.n_book_crossed > 0:
        d = min(20.0, audit.n_book_crossed * 5.0)
        score -= d
        deductions.append(("book_crossed", d, f"{audit.n_book_crossed} crossed bid>=ask events"))

    # long gaps (per 1000 bars 比例)
    if audit.n_l2book > 0:
        gap_rate = audit.n_long_gaps_30s / max(audit.n_l2book / 1000.0, 1.0)
        if gap_rate > 1:
            d = min(10.0, gap_rate * 2.0)
            score -= d
            deductions.append(
                ("long_gaps", d, f"{audit.n_long_gaps_30s} gaps>30s ({gap_rate:.1f}/k bars)")
            )

    # mid jumps (>5% は重大)
    if audit.n_mid_jumps_over_5pct > 0:
        d = min(15.0, audit.n_mid_jumps_over_5pct * 5.0)
        score -= d
        deductions.append(("mid_jump_5pct", d, f"{audit.n_mid_jumps_over_5pct} mid jumps >5%"))

    return max(score, 0.0), deductions


def _score_external(
    bench: ExternalBenchmark | None,
    *,
    market_closed_excuse: bool = False,
) -> tuple[float, list[tuple[str, float, str]]]:
    """外部突合 0-30 のうちでスコアリング.

    market_closed_excuse=True (現在が外部市場の closure 時間) なら,
    benchmark 取得不能による減点を抑制 (期待動作).
    """
    score = 30.0
    deductions: list[tuple[str, float, str]] = []

    if bench is None or bench.n_aligned == 0:
        if market_closed_excuse:
            # closure 中は benchmark が取れなくて当然 → 5pt のみ減点
            score -= 5.0
            deductions.append(("external_market_closed", 5.0, "external market closed (expected)"))
        else:
            score -= 15.0
            deductions.append(("external_no_data", 15.0, "no external benchmark data aligned"))
        return score, deductions

    # corr < 0.95 で減点 (BTC は >0.95 期待)
    if bench.correlation < 0.95:
        d = min(15.0, (0.95 - bench.correlation) * 100.0)
        score -= d
        deductions.append(("external_corr_low", d, f"corr {bench.correlation:.4f} < 0.95"))

    # median diff bps > 5 で減点 (HL oracle が systematically ずれてる)
    if abs(bench.median_diff_bps) > 5.0:
        d = min(10.0, (abs(bench.median_diff_bps) - 5.0) / 1.0)
        score -= d
        deductions.append(
            ("external_median_diff", d, f"median diff {bench.median_diff_bps:+.1f}bps > 5bps")
        )

    return max(score, 0.0), deductions


def _is_xyz_market_closed_now() -> bool:
    """現在が xyz dex の元市場 (US 株式) の closure 時間帯か.

    R2 (週末) / R3 (CMEメンテ) / R4 (祝日) なら xyz の SPY/QQQ benchmark は取得困難.
    """
    from src.config import RegimeConfig

    cfg = RegimeConfig()
    regime = classify_regime(datetime.now(UTC), cfg)
    return regime != Regime.ACTIVE


def compute_quality_score(
    internal: AuditReport,
    external: ExternalAuditReport | None,
) -> dict[str, QualityScoreCard]:
    """各 symbol について 0-100 のスコアを算出."""
    cards: dict[str, QualityScoreCard] = {}
    bench_by_symbol: dict[str, ExternalBenchmark] = {}
    if external:
        for b in external.benchmarks:
            bench_by_symbol[b.symbol] = b

    market_closed = _is_xyz_market_closed_now()
    for sym, audit in internal.by_symbol.items():
        s_int, d_int = _score_internal(audit)
        bench = bench_by_symbol.get(sym)
        # xyz: prefix の銘柄は SPY/QQQ benchmark に依存するので closure 中は減点抑制
        excuse = market_closed and sym.startswith("xyz:")
        s_ext, d_ext = _score_external(bench, market_closed_excuse=excuse)
        total = s_int + s_ext
        cards[sym] = QualityScoreCard(symbol=sym, score=total, deductions=d_int + d_ext)
    return cards


def render_markdown(cards: dict[str, QualityScoreCard]) -> str:
    lines: list[str] = []
    lines.append("# Audit-D: data quality score")
    lines.append("")
    lines.append("0-100 score (internal 70 + external 30). 95 未満で warning, 80 未満で critical.")
    lines.append("")
    lines.append("## summary")
    lines.append("")
    lines.append("| symbol | score | warning |")
    lines.append("|---|---|---|")
    for sym, card in cards.items():
        warn = "⚠" if card.is_warning else "✓"
        lines.append(f"| {sym} | {card.score:.1f} | {warn} |")
    lines.append("")

    for sym, card in cards.items():
        if not card.deductions:
            continue
        lines.append(f"## {sym} deductions")
        lines.append("")
        lines.append("| rule | points | message |")
        lines.append("|---|---|---|")
        for rule, pts, msg in card.deductions:
            lines.append(f"| {rule} | -{pts:.1f} | {msg} |")
        lines.append("")
    return "\n".join(lines)
