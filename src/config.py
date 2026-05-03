"""設定管理 (Pydantic Settings + YAML).

config/default.yaml をベースに、config/local.yaml で上書き、最後に環境変数で上書きする。
"""

from __future__ import annotations

from pathlib import Path
from typing import Literal

import yaml
from pydantic import BaseModel, Field
from pydantic_settings import BaseSettings, SettingsConfigDict


class HyperliquidConfig(BaseModel):
    info_api_url: str = "https://api.hyperliquid.xyz/info"
    ws_url: str = "wss://api.hyperliquid.xyz/ws"
    symbols: list[str] = Field(default_factory=lambda: ["SP500", "XYZ100", "BTC", "ETH"])
    rest_poll_interval_sec: int = 60
    l2book_snapshot_interval_sec: int = 5
    l2book_levels: int = 10
    ws_reconnect_max_attempts: int = 100
    ws_reconnect_backoff_initial_sec: float = 1.0
    ws_reconnect_backoff_max_sec: float = 60.0


class StorageConfig(BaseModel):
    raw_data_root: Path = Path("data/raw")
    curated_data_root: Path = Path("data/curated")
    parquet_compression: Literal["zstd", "snappy", "gzip"] = "zstd"


class RegimeConfig(BaseModel):
    """Regime境界設定 (v3 §4.2 Gemini指摘の boundary buffer)."""

    # US Equities active session: Sun 20:00 ET ~ Fri 20:00 ET
    active_start_weekday: int = 6  # Sunday (0=Mon, 6=Sun)
    active_start_hour_et: int = 20
    active_end_weekday: int = 4  # Friday
    active_end_hour_et: int = 20
    # CME daily maintenance: 17:00-18:00 ET
    cme_maintenance_start_hour_et: int = 17
    cme_maintenance_end_hour_et: int = 18
    # Boundary buffer: regime境界±N分は regime_uncertain=true
    boundary_buffer_minutes: int = 5


class CostConfig(BaseModel):
    """コストパラメータ (v3 §4.3)."""

    taker_fee_rate: float = 0.00045  # 0.045%
    funding_multiplier: float = 0.5  # 米株 perp は 0.5x dampening
    # 標準 funding interest rate component: 0.01%/8h
    base_interest_rate_per_8h: float = 0.0001


class LoggingConfig(BaseModel):
    level: Literal["DEBUG", "INFO", "WARNING", "ERROR"] = "INFO"
    json_output: bool = True
    heartbeat_interval_sec: int = 300  # 5 min


class AppConfig(BaseSettings):
    """全体 config. 環境変数 DON_<FIELD> で override 可能."""

    model_config = SettingsConfigDict(
        env_prefix="DON_",
        env_nested_delimiter="__",
        case_sensitive=False,
    )

    hyperliquid: HyperliquidConfig = Field(default_factory=HyperliquidConfig)
    storage: StorageConfig = Field(default_factory=StorageConfig)
    regime: RegimeConfig = Field(default_factory=RegimeConfig)
    cost: CostConfig = Field(default_factory=CostConfig)
    logging: LoggingConfig = Field(default_factory=LoggingConfig)


def load_config(yaml_paths: list[Path] | None = None) -> AppConfig:
    """YAML を順に読んで上書き → 環境変数で更に上書き."""
    merged: dict = {}
    for p in yaml_paths or []:
        if p.exists():
            with p.open("r", encoding="utf-8") as f:
                data = yaml.safe_load(f) or {}
                _deep_merge(merged, data)
    return AppConfig(**merged)


def _deep_merge(base: dict, overrides: dict) -> None:
    for k, v in overrides.items():
        if k in base and isinstance(base[k], dict) and isinstance(v, dict):
            _deep_merge(base[k], v)
        else:
            base[k] = v
