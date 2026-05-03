"""統合 CLI (typer)."""

from __future__ import annotations

import asyncio
from datetime import date as date_t
from pathlib import Path

import typer

from src.config import load_config
from src.logging_setup import setup_logging

app = typer.Typer(help="diff-old-new CLI")


def _load() -> tuple:
    cfg = load_config(
        [
            Path("config/default.yaml"),
            Path("config/local.yaml"),
        ]
    )
    setup_logging(cfg.logging)
    return cfg


@app.command()
def collect() -> None:
    """L1: HL から WS+REST でデータ収集 (Ctrl-C で停止)."""
    from src.l1_collector.runner import L1Runner

    cfg = _load()
    runner = L1Runner(cfg)
    asyncio.run(runner.run())


@app.command()
def features(
    day: str | None = typer.Option(None, help="ISO date 'YYYY-MM-DD'. 指定なしで全期間"),
) -> None:
    """L2: data/raw → data/curated/features."""
    from src.l2_features.pipeline import run_pipeline

    cfg = _load()
    parsed = date_t.fromisoformat(day) if day else None
    run_pipeline(cfg, parsed)


@app.command()
def backtest(
    strategy: str = typer.Argument("h1", help="strategy id"),
    symbol: str = typer.Option("SP500", help="対象 symbol"),
    day: str | None = typer.Option(None, help="ISO date"),
    exit_min: int = typer.Option(60, help="exit_after_minutes"),
) -> None:
    """L3: 指定戦略を curated features に対して backtest."""
    from src.l2_features.loader import load_table
    from src.l3_strategy.backtest import BacktestEngine
    from src.l3_strategy.strategies.h1_closure_mean_rev import (
        H1ClosureMeanReversion,
    )

    cfg = _load()
    parsed = date_t.fromisoformat(day) if day else None

    # curated 側の features を読む
    cfg_curated = cfg.storage.model_copy(update={"raw_data_root": cfg.storage.curated_data_root})
    df = load_table("features", cfg_curated, parsed)

    if strategy == "h1":
        strat = H1ClosureMeanReversion()
    else:
        raise typer.BadParameter(f"Unknown strategy: {strategy}")

    engine = BacktestEngine(cfg.cost)
    result = engine.run(strat, df, symbol_filter=symbol, exit_after_minutes=exit_min)

    print(
        f"[{result.strategy_name}] symbol={symbol} "
        f"N={result.n_trades} "
        f"net_pnl=${result.net_pnl_usd:.2f} "
        f"mean={result.mean_net_bps:+.2f}bps "
        f"se={result.se_bps:.2f}bps "
        f"win_rate={result.win_rate * 100:.1f}%"
    )
    if result.n_trades >= 30:
        # CLT 95% 信頼区間
        ci_low = result.mean_net_bps - 1.96 * result.se_bps
        ci_high = result.mean_net_bps + 1.96 * result.se_bps
        print(f"95% CI: [{ci_low:+.2f}, {ci_high:+.2f}] bps")
        if ci_low > 0:
            print("[OK] コスト控除後期待値が95%信頼区間で正 (LLN/CLT 採否判定)")
        else:
            print("[NG] サンプル不足 or エッジ未確認")


if __name__ == "__main__":
    app()
