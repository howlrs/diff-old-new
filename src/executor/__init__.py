"""Python connector for the Rust `executor-server`.

Thin async client wrapping the REST + WS API. The strategy layer (`l3_strategy`)
hands intents to this module instead of touching Hyperliquid directly.

Usage::

    from src.executor import ExecutorClient, Intent, Algorithm

    async with ExecutorClient("http://127.0.0.1:8085") as cli:
        exec_id = await cli.start(
            algorithm=Algorithm.MARKET,
            symbol="BTC",
            intent=Intent.OPEN,
            target_size="0.1",
        )
        async for event in cli.stream(exec_id):
            print(event)
"""

from __future__ import annotations

from .client import (
    Algorithm,
    ExecutionStatus,
    ExecutorClient,
    ExecutorClientError,
    Intent,
)

__all__ = [
    "Algorithm",
    "ExecutionStatus",
    "ExecutorClient",
    "ExecutorClientError",
    "Intent",
]
