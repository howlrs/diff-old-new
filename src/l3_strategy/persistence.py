"""BacktestResult の Parquet 永続化.

GUI 側 (notebooks/dashboard.py) が読むための共通 schema を定義する.
パス: data/curated/backtest_results/{strategy}/{run_id}.parquet
"""

from __future__ import annotations

import uuid
from datetime import UTC, datetime
from pathlib import Path

from src.config import StorageConfig
from src.l1_collector.storage import write_parquet_atomic
from src.l3_strategy.backtest import BacktestResult
from src.logging_setup import get_logger

log = get_logger("l3.persist")


def trades_to_rows(result: BacktestResult, run_meta: dict) -> list[dict]:
    """BacktestResult.trades を Parquet 用の row dict 列に変換."""
    rows: list[dict] = []
    for t in result.trades:
        size = max(abs(t.size_usd), 1e-9)
        net_bps = t.net_pnl_usd / size * 10000.0
        gross_bps = t.gross_pnl_usd / size * 10000.0
        rows.append(
            {
                "run_id": run_meta["run_id"],
                "strategy_name": result.strategy_name,
                "symbol": t.symbol,
                "side": t.side,
                "entry_ts": t.entry_ts,
                "exit_ts": t.exit_ts,
                "size_usd": t.size_usd,
                "entry_px": t.entry_px,
                "exit_px": t.exit_px,
                "gross_pnl_usd": t.gross_pnl_usd,
                "cost_usd": t.cost_usd,
                "net_pnl_usd": t.net_pnl_usd,
                "gross_bps": gross_bps,
                "net_bps": net_bps,
                "holding_minutes": t.holding_minutes,
                # run metadata (各 trade row にも埋めて join 不要に)
                "run_started_at": run_meta["run_started_at"],
                "run_symbol_filter": run_meta.get("symbol_filter"),
                "run_exit_after_minutes": run_meta.get("exit_after_minutes"),
                "run_taker_fee_rate": run_meta.get("taker_fee_rate"),
                "run_funding_multiplier": run_meta.get("funding_multiplier"),
            }
        )
    return rows


def save_backtest_result(
    result: BacktestResult,
    cfg: StorageConfig,
    *,
    symbol_filter: str | None,
    exit_after_minutes: int,
    taker_fee_rate: float,
    funding_multiplier: float,
) -> Path | None:
    """BacktestResult を data/curated/backtest_results/{strategy}/ 以下に Parquet 保存."""
    run_id = uuid.uuid4().hex[:12]
    run_meta = {
        "run_id": run_id,
        "run_started_at": datetime.now(UTC),
        "symbol_filter": symbol_filter,
        "exit_after_minutes": exit_after_minutes,
        "taker_fee_rate": taker_fee_rate,
        "funding_multiplier": funding_multiplier,
    }
    rows = trades_to_rows(result, run_meta)
    if not rows:
        log.warning("backtest.persist.empty", strategy=result.strategy_name, run_id=run_id)
        return None

    table = f"backtest_results/{result.strategy_name}"
    out = write_parquet_atomic(rows, table, cfg, use_curated=True)
    log.info(
        "backtest.persist.saved",
        strategy=result.strategy_name,
        run_id=run_id,
        n_trades=len(rows),
        path=str(out) if out else None,
    )
    return out
