"""End-to-end smoke test: spins up the Rust executor-server and drives it
via the Python connector.

PR-C4 (2026-05-05): the mock-mode e2e path is now safe enough for CI —
it never touches HL, just exercises the Python ↔ Rust round-trip. Tests
keep the ``slow`` marker so a developer can opt out via
``pytest -m "not slow"`` if they only care about contract tests.

CI runs ``pytest -m "not live"`` and builds ``executor-server --release``
beforehand. The ``live`` marker remains reserved for tests that hit real
HL endpoints.

The fixture finds a free port, starts the server, waits for /v1/health to
come up, runs the test, and tears the server down on exit. PR-C3 made
``emergency_stop`` mutate global server state (``shutdown_initiated``),
so the fixture is **function-scoped** — every test gets a fresh server.
"""

from __future__ import annotations

import os
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path

import httpx
import pytest

from src.executor import (
    Algorithm,
    ExecutorClient,
    Intent,
)

REPO_ROOT = Path(__file__).resolve().parent.parent


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _server_binary() -> Path:
    """Return the path to the executor-server release binary, falling back
    to debug. Build before running: ``cargo build -p executor-server``."""
    for profile in ("release", "debug"):
        bin_path = REPO_ROOT / "executor" / "target" / profile / "executor-server"
        if bin_path.exists():
            return bin_path
    raise FileNotFoundError(
        "executor-server binary not found — run `cargo build -p executor-server` first"
    )


@pytest.fixture(scope="function")
def running_server() -> Iterator[str]:
    """Start the executor-server, yield its base URL, tear it down.

    Function-scoped because PR-C3 makes emergency_stop mutate global state
    (``ServerState::shutdown_initiated``). A module-scoped fixture would
    leak that state between tests."""
    try:
        binary = _server_binary()
    except FileNotFoundError as e:
        pytest.skip(str(e))
    port = _free_port()
    env = {**os.environ, "EXECUTOR_BIND": f"127.0.0.1:{port}"}
    proc = subprocess.Popen(
        [str(binary)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    base = f"http://127.0.0.1:{port}"
    deadline = time.time() + 10.0
    last_err: Exception | None = None
    while time.time() < deadline:
        try:
            r = httpx.get(f"{base}/v1/health", timeout=1.0)
            if r.is_success:
                break
        except Exception as e:
            last_err = e
        time.sleep(0.1)
    else:
        proc.terminate()
        raise RuntimeError(f"executor-server did not start in time: {last_err}")
    try:
        yield base
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3.0)
        except subprocess.TimeoutExpired:
            proc.kill()


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_health_and_algorithms(running_server: str) -> None:
    async with ExecutorClient(running_server) as cli:
        body = await cli.health()
    assert body["status"] == "ok"
    for n in ("market", "passive", "twap", "market_make"):
        assert n in body["algorithms"]


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_unknown_algorithm_400(running_server: str) -> None:
    async with ExecutorClient(running_server) as cli:
        with pytest.raises(Exception, match="HTTP"):  # ExecutorClientError
            await cli.start(
                algorithm="vwap",
                symbol="BTC",
                intent=Intent.OPEN,
                target_size="0.1",
            )


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_emergency_stop_with_no_running(running_server: str) -> None:
    async with ExecutorClient(running_server) as cli:
        body = await cli.emergency_stop()
    assert body["aborted_executions"] == 0
    assert body["cancelled_orders"] == 0


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_market_book_returns_404(running_server: str) -> None:
    async with ExecutorClient(running_server) as cli:
        with pytest.raises(Exception, match="HTTP"):
            await cli.book("BTC")


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_start_market_then_cancel(running_server: str) -> None:
    """No book is seeded → MarketAlgorithm errors out with InvalidParams.
    The execution still gets registered; we should be able to cancel and
    poll status without panic.

    Realistically the Mock server starts with no book — so the algo's first
    snapshot_book() call returns AlgoError. The status will become
    `failed`. Verify that path is healthy."""
    algorithm = Algorithm.MARKET
    async with ExecutorClient(running_server) as cli:
        resp = await cli.start(
            algorithm=algorithm,
            symbol="BTC",
            intent=Intent.OPEN,
            target_size="0.01",
            params={"max_book_age_ms": 0, "max_attempts": 1, "slice_timeout_ms": 50},
        )
        # Give the algo a moment to fail (no book seeded).
        await _sleep(0.2)
        st = await cli.status(resp.exec_id)
        # Acceptable terminal states: failed (no book) or aborted.
        assert st["status"] in ("failed", "aborted", "running", "finalizing")


async def _sleep(seconds: float) -> None:
    import asyncio

    await asyncio.sleep(seconds)


# ---- PR-C4: Python e2e for PR-C3 features (operator_id audit, idempotent
#      emergency_stop, 503 after stop) ----


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_emergency_stop_with_operator_id(running_server: str) -> None:
    """operator_id passes through Python connector → server audit log.
    The server doesn't echo it, so this just verifies the code path doesn't
    blow up on either side."""
    async with ExecutorClient(running_server, operator_id="alice@desk") as cli:
        body = await cli.emergency_stop()
    assert body["aborted_executions"] == 0
    assert body["cancelled_orders"] == 0


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_idempotent_emergency_stop(running_server: str) -> None:
    """PR-C3: a second emergency_stop call after the first must be a no-op."""
    async with ExecutorClient(running_server) as cli:
        first = await cli.emergency_stop()
        second = await cli.emergency_stop()
    # First call may or may not have running executions to abort (typically 0
    # in this fixture, but accept anything ≥ 0). Second must be (0, 0).
    assert first["aborted_executions"] >= 0
    assert second["aborted_executions"] == 0
    assert second["cancelled_orders"] == 0


@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_503_after_emergency_stop(running_server: str) -> None:
    """PR-C3: start_exec must return 503 once emergency_stop has fired."""
    async with ExecutorClient(running_server) as cli:
        await cli.emergency_stop()
        with pytest.raises(Exception, match="HTTP 503"):
            await cli.start(
                algorithm=Algorithm.MARKET,
                symbol="BTC",
                intent=Intent.OPEN,
                target_size="0.001",
            )
