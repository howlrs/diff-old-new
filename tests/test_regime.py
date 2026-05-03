"""Regime tagger のテスト (US ET 時間ハンドリングと boundary buffer)."""

from __future__ import annotations

from datetime import datetime

from pendulum import timezone

from src.config import RegimeConfig
from src.l2_features.regime import (
    Regime,
    classify_regime,
    is_near_boundary,
)

ET = timezone("America/New_York")


def _et_to_utc(year: int, month: int, day: int, hour: int, minute: int = 0) -> datetime:
    """ET 時刻 (DST 反映) を UTC へ正確に変換."""
    return ET.convert(datetime(year, month, day, hour, minute)).astimezone(
        __import__("datetime").timezone.utc
    )


def test_active_session_weekday() -> None:
    cfg = RegimeConfig()
    # 月曜 10:00 ET (= UTC 15:00 冬時間)
    ts = _et_to_utc(2026, 5, 4, 10)  # Monday
    assert classify_regime(ts, cfg) == Regime.ACTIVE


def test_cme_daily_maintenance() -> None:
    cfg = RegimeConfig()
    # 月曜 17:30 ET = CME メンテ
    ts = _et_to_utc(2026, 5, 4, 17, 30)
    assert classify_regime(ts, cfg) == Regime.CLOSURE_DAILY


def test_weekend_closure_saturday() -> None:
    cfg = RegimeConfig()
    # 土曜 12:00 ET
    ts = _et_to_utc(2026, 5, 9, 12)  # Saturday
    assert classify_regime(ts, cfg) == Regime.CLOSURE_WEEKEND


def test_friday_evening_after_close() -> None:
    cfg = RegimeConfig()
    # 金曜 21:00 ET (20:00 close 後)
    ts = _et_to_utc(2026, 5, 8, 21)  # Friday
    assert classify_regime(ts, cfg) == Regime.CLOSURE_WEEKEND


def test_sunday_evening_before_open() -> None:
    cfg = RegimeConfig()
    # 日曜 19:30 ET (20:00 open 前)
    ts = _et_to_utc(2026, 5, 10, 19, 30)  # Sunday
    assert classify_regime(ts, cfg) == Regime.CLOSURE_WEEKEND


def test_holiday() -> None:
    cfg = RegimeConfig()
    # Memorial Day 2026-05-25 (Monday) at 12:00 ET
    ts = _et_to_utc(2026, 5, 25, 12)
    assert classify_regime(ts, cfg) == Regime.CLOSURE_HOLIDAY


def test_boundary_buffer_around_cme_maint() -> None:
    cfg = RegimeConfig(boundary_buffer_minutes=10)
    # 16:55 ET (CME メンテ開始 17:00 ET の5分前) → boundary
    ts = _et_to_utc(2026, 5, 4, 16, 55)
    assert is_near_boundary(ts, cfg) is True
    # 14:00 ET → 普通の active session 中央, 境界遠い
    ts2 = _et_to_utc(2026, 5, 4, 14, 0)
    assert is_near_boundary(ts2, cfg) is False
