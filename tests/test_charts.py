"""altair チャート関数の smoke test (戻り値が Chart, 空 input でもエラー無し)."""

from __future__ import annotations

import altair as alt
import polars as pl

from src.gui.charts import (
    book_top_depth_chart,
    equity_curve_chart,
    ipd_time_series_chart,
    trade_pnl_histogram,
    underwater_chart,
)


def test_equity_curve_handles_empty() -> None:
    c = equity_curve_chart(pl.DataFrame())
    assert isinstance(c, alt.Chart)


def test_underwater_handles_empty() -> None:
    c = underwater_chart(pl.DataFrame())
    assert isinstance(c, alt.Chart)


def test_trade_pnl_histogram_empty() -> None:
    c = trade_pnl_histogram(pl.DataFrame())
    assert isinstance(c, alt.Chart)


def test_ipd_time_series_handles_empty() -> None:
    c = ipd_time_series_chart(pl.DataFrame(), "BTC")
    assert isinstance(c, alt.Chart)


def test_book_top_depth_handles_empty() -> None:
    c = book_top_depth_chart(pl.DataFrame(), "BTC")
    assert isinstance(c, alt.Chart)
