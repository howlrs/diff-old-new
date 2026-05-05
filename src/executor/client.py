"""Async REST + WS client for the Rust executor-server.

The Rust server lives in ``executor/`` and exposes the API documented in
``docs/specs/2026-05-04-rust-executor-design.md``. This client is a thin
adapter — no algorithm logic lives here.

Design notes
------------
- The connector is **httpx-based** so we share the project's async HTTP stack.
- ``stream()`` is an async generator over WebSocket frames. Each frame is the
  ``Progress`` payload as defined by the Rust ``executor_core::intent::Progress``
  enum (snake_case ``type`` discriminator).
- All numeric quantities are kept as decimal strings to avoid f64 precision
  loss between Python and Rust.
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from dataclasses import dataclass
from decimal import Decimal
from enum import StrEnum
from typing import Any, Self

import httpx


class Algorithm(StrEnum):
    """Algorithm names accepted by the executor-server router."""

    MARKET = "market"
    PASSIVE = "passive"
    TWAP = "twap"
    MARKET_MAKE = "market_make"


class Intent(StrEnum):
    """Position-change intent (matches Rust ``Intent`` serde rename_all=snake)."""

    OPEN = "open"
    CLOSE = "close"
    SET_TARGET = "set_target"


class ExecutionStatus(StrEnum):
    RUNNING = "running"
    FINALIZING = "finalizing"
    COMPLETED = "completed"
    ABORTED = "aborted"
    FAILED = "failed"


class ExecutorClientError(RuntimeError):
    """Raised when the server returns a non-2xx response."""

    def __init__(self, status: int, body: Any):
        super().__init__(f"HTTP {status}: {body}")
        self.status = status
        self.body = body


@dataclass(slots=True)
class StartResponse:
    exec_id: str
    algorithm: str


class ExecutorClient:
    """Async client for the Rust executor-server.

    Use as an async context manager::

        async with ExecutorClient("http://127.0.0.1:8085") as cli:
            ...
    """

    def __init__(
        self,
        base_url: str,
        operator_id: str | None = None,
        timeout: float = 10.0,
    ):
        self._base_url = base_url.rstrip("/")
        self._operator_id = operator_id
        self._timeout = timeout
        self._client: httpx.AsyncClient | None = None

    async def __aenter__(self) -> Self:
        self._client = httpx.AsyncClient(timeout=self._timeout)
        return self

    async def __aexit__(self, *exc_info: object) -> None:
        if self._client is not None:
            await self._client.aclose()
            self._client = None

    @property
    def _http(self) -> httpx.AsyncClient:
        if self._client is None:
            raise RuntimeError("ExecutorClient must be used as an async context manager")
        return self._client

    # --- REST methods -------------------------------------------------------

    async def health(self) -> dict[str, Any]:
        return await self._get("/v1/health")

    async def positions(self) -> dict[str, Any]:
        return await self._get("/v1/positions")

    async def book(self, symbol: str) -> dict[str, Any]:
        return await self._get(f"/v1/book/{symbol}")

    async def start(
        self,
        algorithm: Algorithm | str,
        symbol: str,
        intent: Intent | str,
        target_size: str | Decimal,
        params: dict[str, Any] | None = None,
    ) -> StartResponse:
        payload = {
            "algorithm": algorithm.value if isinstance(algorithm, Algorithm) else algorithm,
            "symbol": symbol,
            "intent": intent.value if isinstance(intent, Intent) else intent,
            "target_size": str(target_size),
            "params": params or {},
        }
        body = await self._post("/v1/exec", payload)
        return StartResponse(exec_id=body["exec_id"], algorithm=body["algorithm"])

    async def status(self, exec_id: str) -> dict[str, Any]:
        return await self._get(f"/v1/exec/{exec_id}")

    async def cancel(self, exec_id: str) -> dict[str, Any]:
        return await self._post(f"/v1/exec/{exec_id}/cancel", {})

    async def emergency_stop(self) -> dict[str, Any]:
        return await self._post("/v1/emergency_stop", {})

    # --- WebSocket streaming -----------------------------------------------

    @asynccontextmanager
    async def _ws_connect(self, exec_id: str) -> AsyncIterator[Any]:
        """Open a WS connection for the given exec_id. Hides the dependency
        on the optional ``websockets`` package — raises a clear error if it
        isn't installed."""
        try:
            import websockets
        except ImportError as e:
            raise RuntimeError(
                "executor.ExecutorClient.stream() requires 'websockets'. "
                "Install with: pip install websockets~=16.0"
            ) from e
        ws_url = self._base_url.replace("http://", "ws://", 1).replace("https://", "wss://", 1)
        url = f"{ws_url}/v1/exec/{exec_id}/ws"
        async with websockets.connect(url) as conn:
            yield conn

    async def stream(self, exec_id: str) -> AsyncIterator[dict[str, Any]]:
        """Yield Progress events for an execution.

        Closes when the server-side broadcast channel ends or the consumer
        breaks the loop.
        """
        async with self._ws_connect(exec_id) as conn:
            try:
                while True:
                    raw = await conn.recv()
                    if isinstance(raw, bytes):
                        raw = raw.decode("utf-8")
                    yield json.loads(raw)
            except asyncio.CancelledError:
                raise
            except Exception:
                return

    # --- Internals ----------------------------------------------------------

    async def _get(self, path: str) -> dict[str, Any]:
        resp = await self._http.get(f"{self._base_url}{path}")
        return self._unwrap(resp)

    async def _post(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        # PR-C4: every POST audit-tags the caller via X-Operator-ID.
        # GETs are read-only and intentionally untagged.
        headers: dict[str, str] = {}
        if self._operator_id:
            headers["X-Operator-ID"] = self._operator_id
        resp = await self._http.post(
            f"{self._base_url}{path}",
            json=payload,
            headers=headers or None,
        )
        return self._unwrap(resp)

    @staticmethod
    def _unwrap(resp: httpx.Response) -> dict[str, Any]:
        try:
            body = resp.json()
        except ValueError:
            body = resp.text
        if resp.is_success:
            if not isinstance(body, dict):
                raise ExecutorClientError(resp.status_code, body)
            return body
        raise ExecutorClientError(resp.status_code, body)
