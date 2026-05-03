"""diff-old-new analytics dashboard (marimo).

使い方:
    marimo edit notebooks/dashboard.py    # 編集 (探索モード)
    marimo run notebooks/dashboard.py     # 発表 (read-only アプリ)

設計: docs/specs/2026-05-04-gui-design.md
"""

import marimo

__generated_with = "0.23.4"
app = marimo.App(width="medium")


@app.cell
def _imports():
    import sys
    from pathlib import Path

    # ensure project src on path when notebook is run from notebooks/
    repo_root = Path(__file__).resolve().parent.parent
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    import marimo as mo
    import polars as pl

    from src.config import load_config
    from src.gui.charts import (
        book_top_depth_chart,
        equity_curve_chart,
        ipd_time_series_chart,
        trade_pnl_histogram,
        underwater_chart,
    )
    from src.gui.data_access import GuiDataSource
    from src.gui.perf_metrics import (
        compute_perf_stats,
        equity_curve,
        underwater_curve,
    )

    return (
        mo,
        pl,
        load_config,
        GuiDataSource,
        compute_perf_stats,
        equity_curve,
        underwater_curve,
        equity_curve_chart,
        underwater_chart,
        trade_pnl_histogram,
        ipd_time_series_chart,
        book_top_depth_chart,
        repo_root,
    )


@app.cell
def _header(mo):
    mo.md(
        "# diff-old-new — analytics dashboard\n\n"
        "Hyperliquid HIP-3 (Trade[XYZ]) 米株 perp の **Oracle 二重構造** を活用する戦略の\n"
        "パフォーマンスを観測するダッシュボード.\n\n"
        "- 上段: 戦略 backtest の **8 標準 KPI** (Sharpe / Sortino / Max DD / Hit / PF / "
        "Expectancy + Equity / Underwater curve + Trade table)\n"
        "- 中段: 既存 KPI (K1/K2/K7/K8/K9/fat_tail/regime_diff) のサマリ\n"
        "- 下段: 自由探索セル (DuckDB SQL + 任意の図表)\n\n"
        "詳細: `docs/specs/2026-05-04-gui-design.md` / `docs/phase-1-report.md`"
    )
    return


@app.cell
def _data_source(load_config, GuiDataSource, repo_root):
    cfg = load_config(
        [
            repo_root / "config" / "default.yaml",
            repo_root / "config" / "local.yaml",
        ]
    )
    ds = GuiDataSource.from_config(cfg.storage, kpi_dir=repo_root / "docs" / "kpi")
    return cfg, ds


@app.cell
def _controls(mo, ds):
    strategies = ds.list_backtest_strategies() or ["(no backtest results yet)"]
    symbols = ds.list_symbols() or ["(no data yet)"]

    strategy_select = mo.ui.dropdown(
        options=strategies,
        value=strategies[0],
        label="Strategy",
    )
    symbol_select = mo.ui.dropdown(
        options=["(all)", *symbols],
        value="(all)",
        label="Symbol filter",
    )
    mo.hstack([strategy_select, symbol_select])
    return strategy_select, symbol_select


@app.cell
def _load_trades(ds, strategy_select, symbol_select):
    strategy = strategy_select.value
    symbol = None if symbol_select.value == "(all)" else symbol_select.value
    if strategy.startswith("(no"):
        trades = None
    else:
        trades = ds.load_backtest_trades(strategy, symbol=symbol)
        if trades is not None and trades.is_empty():
            trades = None
    return trades, strategy, symbol


@app.cell
def _perf_summary(mo, trades, compute_perf_stats):
    if trades is None:
        ps = None
        _summary_md = mo.md(
            "> ⚠ **No backtest results found**.\n"
            "> 先に CLI で backtest を実行してください:\n"
            "> ```bash\n"
            "> python -m src.cli backtest h1 --symbol xyz:SP500\n"
            "> ```"
        )
    else:
        ps = compute_perf_stats(trades)
        win_pct = f"{ps.hit_rate * 100:.1f}%"
        pf = "∞" if ps.profit_factor == float("inf") else f"{ps.profit_factor:.2f}"
        _summary_md = mo.md(
            f"## Performance summary\n\n"
            f"| metric | value |\n"
            f"|---|---|\n"
            f"| **N trades** | {ps.n_trades} |\n"
            f"| **Total net P/L (USD)** | {ps.total_pnl_usd:+.2f} |\n"
            f"| **Sharpe (annualized)** | **{ps.sharpe_annualized:+.2f}** &nbsp; "
            f"95% CI [{ps.sharpe_ci_low:+.2f}, {ps.sharpe_ci_high:+.2f}] |\n"
            f"| **Sortino (annualized)** | {ps.sortino_annualized:+.2f} |\n"
            f"| **Max Drawdown** | {ps.max_drawdown_pct:.2%} |\n"
            f"| **Calmar** | {ps.calmar:+.2f} |\n"
            f"| **Hit rate** | {win_pct} |\n"
            f"| **Profit factor** | {pf} |\n"
            f"| **Expectancy (bps/trade)** | {ps.expectancy_bps:+.2f} |\n"
            f"| **σ (bps/trade)** | {ps.std_bps:.2f} |"
        )
    _summary_md
    return (ps,)


@app.cell
def _equity_chart(mo, trades, equity_curve, equity_curve_chart):
    if trades is None:
        mo.md("")
        chart_eq = None
    else:
        eq = equity_curve(trades)
        chart_eq = mo.ui.altair_chart(equity_curve_chart(eq))
    chart_eq if chart_eq is not None else mo.md("")
    return (chart_eq,)


@app.cell
def _underwater_chart(mo, trades, underwater_curve, underwater_chart):
    if trades is None:
        mo.md("")
        chart_uw = None
    else:
        uw = underwater_curve(trades)
        chart_uw = mo.ui.altair_chart(underwater_chart(uw))
    chart_uw if chart_uw is not None else mo.md("")
    return (chart_uw,)


@app.cell
def _trade_histogram(mo, trades, trade_pnl_histogram):
    if trades is None:
        mo.md("")
        hist = None
    else:
        hist = mo.ui.altair_chart(trade_pnl_histogram(trades))
    hist if hist is not None else mo.md("")
    return (hist,)


@app.cell
def _trade_table(mo, trades):
    if trades is None:
        mo.md("")
        table = None
    else:
        cols = [
            "entry_ts",
            "exit_ts",
            "symbol",
            "side",
            "size_usd",
            "entry_px",
            "exit_px",
            "net_pnl_usd",
            "net_bps",
            "holding_minutes",
        ]
        avail = [c for c in cols if c in trades.columns]
        table = mo.ui.table(
            trades.select(avail).sort("entry_ts", descending=True).head(200),
            label="Recent 200 trades (sortable)",
        )
    table if table is not None else mo.md("")
    return (table,)


@app.cell
def _audit_section(mo, cfg, repo_root):
    """データ品質スコア (Audit-D 結果) を表示."""
    audit_md_path = repo_root / "docs" / "audit" / "D_quality_score.md"
    if audit_md_path.exists():
        body = audit_md_path.read_text(encoding="utf-8")
        _audit_view = mo.md(body)
    else:
        _audit_view = mo.md(
            "## Audit\n\n"
            "_audit レポート未生成. 以下を実行してから再表示してください:_\n\n"
            "```bash\n"
            "python scripts/audit_quality.py\n"
            "```"
        )
    _ = cfg
    _audit_view
    return


@app.cell
def _kpi_cards(mo, ds):
    kpi_names = ["K1", "K2", "K7", "K8", "K9", "fat_tail", "regime_diff"]
    cards = []
    for name in kpi_names:
        data = ds.load_kpi_json(name)
        if not data:
            cards.append(
                mo.md(f"### {name}\n\n_no data yet_").style(
                    {
                        "min-width": "200px",
                        "padding": "8px",
                        "border": "1px solid #ccc",
                        "border-radius": "8px",
                    }
                )
            )
            continue
        # 短いサマリ表示
        summary_lines: list[str] = []
        if "warning" in data:
            summary_lines.append(f"⚠ {data['warning']}")
        if "n_total_bars" in data:
            summary_lines.append(f"bars: {data['n_total_bars']}")
        if "n_segments" in data:
            summary_lines.append(f"segments: {data['n_segments']}")
        if "n_total_events" in data:
            summary_lines.append(f"events: {data['n_total_events']}")
        if "by_symbol" in data:
            for sym, stat in data["by_symbol"].items():
                if isinstance(stat, dict) and "n_events" in stat:
                    rate = stat.get("recovery_rate", 0)
                    p95 = stat.get("recovery_sec_distribution", {}).get("p95", "-")
                    p95s = f"{p95:.1f}s" if isinstance(p95, (int, float)) else str(p95)
                    summary_lines.append(
                        f"{sym}: n={stat['n_events']} rate={rate * 100:.0f}% p95={p95s}"
                    )
        body = "\n\n".join(summary_lines) if summary_lines else "(see md)"
        cards.append(
            mo.md(f"### {name}\n\n{body}").style(
                {
                    "min-width": "240px",
                    "padding": "8px",
                    "border": "1px solid #ccc",
                    "border-radius": "8px",
                }
            )
        )
    mo.hstack(cards, wrap=True, gap=0.5)
    return


@app.cell
def _exploration_header(mo):
    mo.md(
        "---\n\n"
        "## 自由探索\n\n"
        "以下のセルを編集して任意の分析を実行できる. `con = ds.con` を使えば DuckDB SQL で\n"
        "Parquet を直接 query できる."
    )
    return


@app.cell
def _explore_ipd(mo, ds, symbol_select, ipd_time_series_chart):
    sym_for_ipd = symbol_select.value if symbol_select.value != "(all)" else "xyz:SP500"
    features = ds.load_features(sym_for_ipd)
    if features.is_empty():
        mo.md("_no features for ipd plot yet_")
        ipd_chart = None
    else:
        ipd_chart = mo.ui.altair_chart(ipd_time_series_chart(features, sym_for_ipd))
    ipd_chart if ipd_chart is not None else mo.md("_(empty)_")
    return (ipd_chart,)


@app.cell
def _explore_book(mo, ds, symbol_select, book_top_depth_chart):
    sym_for_book = symbol_select.value if symbol_select.value != "(all)" else "xyz:SP500"
    l2 = ds.load_l2book(sym_for_book)
    if l2.is_empty():
        mo.md("_no l2book for top-of-book chart yet_")
        book_chart = None
    else:
        book_chart = mo.ui.altair_chart(book_top_depth_chart(l2, sym_for_book))
    book_chart if book_chart is not None else mo.md("_(empty)_")
    return (book_chart,)


@app.cell
def _explore_sql(mo, ds):
    _ = ds  # marimo dependency tracking 用 (con を変えたら再評価する想定)
    mo.md(
        "### DuckDB SQL 探索\n\n"
        "`ds.con` で SQL を直接実行できる. 例:\n\n"
        "```python\n"
        "df = ds.con.execute(\n"
        "    \"SELECT symbol, COUNT(*) FROM read_parquet('data/raw/l2book/**/*.parquet', union_by_name=true) GROUP BY symbol\"\n"
        ").pl()\n"
        "df\n"
        "```"
    )
    return


if __name__ == "__main__":
    app.run()
