"""Regime tagger: R1 (active) / R2 (closure-weekend) / R3 (closure-daily) / R4 (closure-holiday).

v3 §1.2 / §4.2 の Gemini 指摘 (boundary buffer) を実装.
Issue #28 (Phase 1.5) 反映: pandas_market_calendars で NYSE 祝日を動的に取得.
"""

from __future__ import annotations

from datetime import UTC, date, datetime, time, timedelta
from enum import StrEnum

import polars as pl
from pendulum import timezone as pdl_tz

from src.config import RegimeConfig
from src.l2_features.calendar import get_nyse_early_close_dates, get_nyse_holidays

ET = pdl_tz("America/New_York")


class Regime(StrEnum):
    """v3 §1.2 で定義された regime."""

    ACTIVE = "R1_active"
    CLOSURE_WEEKEND = "R2_closure_weekend"
    CLOSURE_DAILY = "R3_closure_daily"  # CME メンテ 17-18 ET
    CLOSURE_HOLIDAY = "R4_closure_holiday"  # NYSE full holiday or early close
    LIMIT_MOVE = "R5_limit_move"  # Phase 2 で精緻化
    ROLLOVER = "R6_rollover"  # Phase 2 で精緻化


def _holidays_lazy() -> set[date]:
    """pandas_market_calendars 由来の動的祝日 (年4回 cache lookup)."""
    return get_nyse_holidays()


def classify_regime(
    ts_utc: datetime,
    cfg: RegimeConfig,
    holidays: set[date] | None = None,
) -> Regime:
    """単一 timestamp を regime に分類.

    優先順位:
        1. holiday → CLOSURE_HOLIDAY
        2. CME daily maintenance (17-18 ET) → CLOSURE_DAILY
        3. weekend (Fri 20:00 ET 〜 Sun 20:00 ET) → CLOSURE_WEEKEND
        4. それ以外 → ACTIVE
    """
    # Convert UTC to ET (handles DST automatically)
    if ts_utc.tzinfo is None:
        ts_utc = ts_utc.replace(tzinfo=UTC)
    ts_et = ts_utc.astimezone(ET)
    et_date = ts_et.date()
    et_time = ts_et.time()
    et_weekday = ts_et.weekday()  # 0=Mon ... 6=Sun

    if holidays is None:
        holidays = _holidays_lazy()

    if et_date in holidays:
        return Regime.CLOSURE_HOLIDAY

    # NYSE early close 日 (例: Black Friday 13:00 ET) 以降は holiday 扱い
    early = get_nyse_early_close_dates().get(et_date)
    if early is not None and et_time >= early:
        return Regime.CLOSURE_HOLIDAY

    # CME daily maintenance (17:00-18:00 ET, 平日のみ)
    if et_weekday < 5 and time(cfg.cme_maintenance_start_hour_et) <= et_time < time(
        cfg.cme_maintenance_end_hour_et
    ):
        return Regime.CLOSURE_DAILY

    # Weekend boundaries:
    # active = Sun 20:00 ET ~ Fri 20:00 ET (with weekday wraparound)
    if _is_in_weekend_closure(et_weekday, et_time, cfg):
        return Regime.CLOSURE_WEEKEND

    return Regime.ACTIVE


def _is_in_weekend_closure(
    weekday: int,
    t: time,
    cfg: RegimeConfig,
) -> bool:
    """Fri 20:00 ET 以降, 土曜日終日, 日 20:00 ET より前 を週末 closure とみなす."""
    fri_close = time(cfg.active_end_hour_et)
    sun_open = time(cfg.active_start_hour_et)
    if weekday == 4 and t >= fri_close:  # Friday after 20:00 ET
        return True
    if weekday == 5:  # Saturday all day
        return True
    return weekday == 6 and t < sun_open  # Sunday before 20:00 ET


def is_near_boundary(
    ts_utc: datetime,
    cfg: RegimeConfig,
) -> bool:
    """境界±buffer 内なら True (regime_uncertain フラグに使う).

    実装: 現在の regime と ±buffer min 後の regime が異なれば境界近傍.
    """
    delta = timedelta(minutes=cfg.boundary_buffer_minutes)
    base = classify_regime(ts_utc, cfg)
    after = classify_regime(ts_utc + delta, cfg)
    before = classify_regime(ts_utc - delta, cfg)
    return base != after or base != before


def tag_dataframe(
    df: pl.DataFrame,
    ts_col: str,
    cfg: RegimeConfig,
) -> pl.DataFrame:
    """Polars DataFrame に regime と regime_uncertain カラムを付与.

    ts_col は UTC datetime カラムを想定.
    """
    ts_series = df[ts_col]
    regimes: list[str] = []
    uncertain: list[bool] = []
    for ts in ts_series:
        if ts is None:
            regimes.append(Regime.ACTIVE.value)
            uncertain.append(True)
            continue
        if isinstance(ts, datetime):
            ts_utc = ts if ts.tzinfo else ts.replace(tzinfo=UTC)
        else:
            ts_utc = datetime.fromisoformat(str(ts)).replace(tzinfo=UTC)
        regimes.append(classify_regime(ts_utc, cfg).value)
        uncertain.append(is_near_boundary(ts_utc, cfg))
    return df.with_columns(
        [
            pl.Series("regime", regimes),
            pl.Series("regime_uncertain", uncertain),
        ]
    )
