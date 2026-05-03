"""Dynamic market calendar (NYSE / CME) — Issue #28.

`pandas_market_calendars` を使い, NYSE / CME futures (CMES_GLOBEX) の
祝日カレンダーと early close / late open を取得する.

Phase 1 では年単位で 1 度キャッシュ. 過去N年 + 未来1年を持つ.
"""

from __future__ import annotations

from datetime import UTC, date, datetime, time, timedelta
from functools import lru_cache

import pandas as pd
import pandas_market_calendars as mcal


@lru_cache(maxsize=1)
def get_nyse_holidays(start_year: int = 2024, end_year: int = 2027) -> set[date]:
    """NYSE 完全休場の date set."""
    cal = mcal.get_calendar("NYSE")
    schedule = cal.schedule(start_date=f"{start_year}-01-01", end_date=f"{end_year}-12-31")
    open_days = {ts.date() for ts in schedule.index}
    all_days: set[date] = set()
    d = date(start_year, 1, 1)
    end_d = date(end_year, 12, 31)
    while d <= end_d:
        all_days.add(d)
        d += timedelta(days=1)
    # holiday = カレンダーに含まれない営業日候補 (土日を除く)
    holidays: set[date] = set()
    for day in all_days:
        if day.weekday() < 5 and day not in open_days:
            holidays.add(day)
    return holidays


@lru_cache(maxsize=1)
def get_nyse_early_close_dates(start_year: int = 2024, end_year: int = 2027) -> dict[date, time]:
    """NYSE early close 日とその close 時刻 (ET).

    Returns:
        { date: close_time_et }  — 普通日は含まれない (13:00 ET など half-day のみ)
    """
    cal = mcal.get_calendar("NYSE")
    schedule = cal.schedule(start_date=f"{start_year}-01-01", end_date=f"{end_year}-12-31")
    if "market_close" not in schedule.columns:
        return {}
    closes = schedule["market_close"]
    early: dict[date, time] = {}
    et_tz = "America/New_York"
    for ts, close in closes.items():
        et_close = close.tz_convert(et_tz) if close.tzinfo else close.tz_localize(et_tz)
        # 通常の close は 16:00 ET. それより早ければ early close.
        if et_close.hour < 16:
            early[ts.date()] = et_close.time()
    return early


def is_holiday(ts_utc: datetime) -> bool:
    if ts_utc.tzinfo is None:
        ts_utc = ts_utc.replace(tzinfo=UTC)
    et_date = pd.Timestamp(ts_utc).tz_convert("America/New_York").date()
    return et_date in get_nyse_holidays()


def get_early_close_for(ts_utc: datetime) -> time | None:
    if ts_utc.tzinfo is None:
        ts_utc = ts_utc.replace(tzinfo=UTC)
    et_date = pd.Timestamp(ts_utc).tz_convert("America/New_York").date()
    return get_nyse_early_close_dates().get(et_date)
