"""L2 統合 pipeline: data/raw → data/curated/features.

simple driver: l2book + asset_ctxs を timestamp で結合し,
regime tag, IPD, EMA, gap features を一気に算出して Parquet 出力.
"""

from __future__ import annotations

from datetime import date as date_t
from pathlib import Path

import polars as pl

from src.config import AppConfig
from src.l1_collector.storage import write_parquet_atomic
from src.l2_features.gap_detector import detect_regime_transitions
from src.l2_features.ipd import EmaState, compute_ipd, step_ema
from src.l2_features.loader import load_asset_ctxs, load_l2book
from src.l2_features.regime import tag_dataframe
from src.logging_setup import get_logger

log = get_logger("l2.pipeline")


def _join_l2_with_ctxs(
    df_l2: pl.DataFrame,
    df_ctxs: pl.DataFrame,
) -> pl.DataFrame:
    """l2book に直近の asset_ctx (oracle/funding/impact) を as-of join."""
    if df_l2.is_empty() or df_ctxs.is_empty():
        return pl.DataFrame()

    a = df_l2.sort("exchange_ts")
    b = df_ctxs.sort("poll_ts").rename({"poll_ts": "ctx_ts"})
    out = a.join_asof(
        b,
        left_on="exchange_ts",
        right_on="ctx_ts",
        by="symbol",
        strategy="backward",
    )
    return out


def _compute_ipd_and_ema(
    df: pl.DataFrame,
) -> pl.DataFrame:
    """銘柄ごとに closure regime のみ EMA 再構築 (Phase 1 簡易版)."""
    if df.is_empty():
        return df

    out_blocks: list[pl.DataFrame] = []
    for sym in df["symbol"].unique().to_list():
        sub = df.filter(pl.col("symbol") == sym).sort("exchange_ts")
        impact_bid = sub["impact_bid_px"].fill_null(strategy="forward")
        impact_ask = sub["impact_ask_px"].fill_null(strategy="forward")
        oracle = sub["oracle_px"]

        ipd_vals: list[float | None] = []
        ema_vals: list[float | None] = []
        st = EmaState()
        for ib, ia, ts, oc in zip(
            impact_bid.to_list(),
            impact_ask.to_list(),
            sub["exchange_ts"].to_list(),
            oracle.to_list(),
            strict=True,
        ):
            if ib is None or ia is None or ts is None:
                ipd_vals.append(None)
                ema_vals.append(None)
                continue
            s = oc if oc is not None else (ib + ia) / 2
            ipd_vals.append(compute_ipd(s, ib, ia))
            ts_sec = ts.timestamp() if hasattr(ts, "timestamp") else 0.0
            ema_vals.append(step_ema(st, ts_sec, ib, ia))

        out_blocks.append(
            sub.with_columns(
                [
                    pl.Series("ipd", ipd_vals),
                    pl.Series("ema_recon", ema_vals),
                ]
            )
        )
    return pl.concat(out_blocks)


def run_pipeline(
    cfg: AppConfig,
    day: date_t | None = None,
) -> Path | None:
    """指定日 (None なら全期間) の features を生成して保存."""
    log.info("l2.pipeline.start", day=str(day))

    df_l2 = load_l2book(cfg.storage, day)
    df_ctxs = load_asset_ctxs(cfg.storage, day)
    if df_l2.is_empty():
        log.warning("l2.pipeline.no_data", reason="empty_l2book")
        return None

    joined = _join_l2_with_ctxs(df_l2, df_ctxs)
    tagged = tag_dataframe(joined, ts_col="exchange_ts", cfg=cfg.regime)
    enriched = _compute_ipd_and_ema(tagged)
    enriched = detect_regime_transitions(enriched)

    rows = enriched.to_dicts()
    out_path = write_parquet_atomic(
        rows,
        "features",
        cfg.storage,
        use_curated=True,
    )
    log.info(
        "l2.pipeline.done",
        rows=len(rows),
        path=str(out_path) if out_path else None,
    )
    return out_path
