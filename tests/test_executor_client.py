"""Python connector for executor-server: smoke + contract tests.

Some tests require the server to be running (live tests, marker `live`).
Others use ``httpx.MockTransport`` to verify the contract without binding
sockets — those run on every CI build.
"""

from __future__ import annotations

import json

import httpx
import pytest

from src.executor import (
    Algorithm,
    ExecutorClient,
    ExecutorClientError,
    Intent,
)


def _make_mock_transport(handler):
    """httpx.MockTransport from a sync handler."""
    return httpx.MockTransport(handler)


def _patched_client(monkeypatch: pytest.MonkeyPatch, handler) -> ExecutorClient:
    """Return an ExecutorClient with httpx.AsyncClient swapped for a mock."""
    transport = _make_mock_transport(handler)
    original_init = httpx.AsyncClient.__init__

    def patched_init(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        kwargs["transport"] = transport
        original_init(self, *args, **kwargs)

    monkeypatch.setattr(httpx.AsyncClient, "__init__", patched_init)
    return ExecutorClient("http://test")


@pytest.mark.asyncio
async def test_health_returns_dict(monkeypatch: pytest.MonkeyPatch) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        assert req.method == "GET"
        assert req.url.path == "/v1/health"
        return httpx.Response(
            200,
            json={
                "status": "ok",
                "algorithms": ["market", "passive", "twap", "market_make"],
                "health": {
                    "ws_connected": False,
                    "ws_reconnect_count": 0,
                    "ws_message_count": 0,
                },
                "running_executions": 0,
            },
        )

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        body = await cli.health()
    assert body["status"] == "ok"
    assert "market" in body["algorithms"]


@pytest.mark.asyncio
async def test_start_serializes_payload(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, object] = {}

    def handler(req: httpx.Request) -> httpx.Response:
        if req.method == "POST" and req.url.path == "/v1/exec":
            captured["body"] = json.loads(req.content)
            return httpx.Response(
                200,
                json={
                    "exec_id": "00000000-0000-0000-0000-000000000001",
                    "algorithm": "MARKET",
                },
            )
        return httpx.Response(404)

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        resp = await cli.start(
            algorithm=Algorithm.MARKET,
            symbol="BTC",
            intent=Intent.OPEN,
            target_size="0.1",
            params={"max_slippage_bps": "20"},
        )
    assert resp.exec_id.startswith("0000")
    body = captured["body"]
    assert isinstance(body, dict)
    assert body["algorithm"] == "market"
    assert body["intent"] == "open"
    assert body["target_size"] == "0.1"
    assert body["params"] == {"max_slippage_bps": "20"}


@pytest.mark.asyncio
async def test_start_accepts_string_enum_aliases(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        body = json.loads(req.content)
        assert body["algorithm"] == "passive"
        assert body["intent"] == "set_target"
        return httpx.Response(
            200,
            json={"exec_id": "00000000-0000-0000-0000-000000000002", "algorithm": "PASSIVE"},
        )

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        await cli.start(
            algorithm="passive",
            symbol="BTC",
            intent="set_target",
            target_size="0.5",
        )


@pytest.mark.asyncio
async def test_cancel_propagates_404(monkeypatch: pytest.MonkeyPatch) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        return httpx.Response(404, json={"code": "not_found", "message": "execution X not found"})

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        with pytest.raises(ExecutorClientError) as excinfo:
            await cli.cancel("missing")
    assert excinfo.value.status == 404


@pytest.mark.asyncio
async def test_emergency_stop_returns_counts(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        assert req.method == "POST"
        assert req.url.path == "/v1/emergency_stop"
        return httpx.Response(
            200,
            json={"aborted_executions": 2, "cancelled_orders": 5},
        )

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        body = await cli.emergency_stop()
    assert body["aborted_executions"] == 2
    assert body["cancelled_orders"] == 5


@pytest.mark.asyncio
async def test_status_returns_running(monkeypatch: pytest.MonkeyPatch) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "exec_id": "00000000-0000-0000-0000-000000000003",
                "algorithm": "TWAP",
                "status": "running",
                "report": None,
                "error": None,
            },
        )

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        body = await cli.status("00000000-0000-0000-0000-000000000003")
    assert body["status"] == "running"


@pytest.mark.asyncio
async def test_book_fetches_ok(monkeypatch: pytest.MonkeyPatch) -> None:
    def handler(req: httpx.Request) -> httpx.Response:
        assert req.url.path == "/v1/book/BTC"
        return httpx.Response(
            200,
            json={"bids": [{"px": "49999", "sz": "1", "n": 1}], "asks": [], "ts": None},
        )

    cli = _patched_client(monkeypatch, handler)
    async with cli:
        book = await cli.book("BTC")
    assert book["bids"][0]["px"] == "49999"


@pytest.mark.asyncio
async def test_client_must_be_used_as_context_manager() -> None:
    cli = ExecutorClient("http://test")
    with pytest.raises(RuntimeError):
        await cli.health()
