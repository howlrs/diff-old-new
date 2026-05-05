# Python connector (`src/executor/`)

Python 戦略レイヤから Rust executor-server を叩くための async クライアント。

> 実装: [`src/executor/client.py`](../../../src/executor/client.py)
> PR: [#65](https://github.com/howlrs/diff-old-new/pull/65)

## インストール

`src/executor/__init__.py` は既に repo に含まれており, `pip install -e ".[dev,all]"`
で追加導入は不要。WebSocket ストリームを使う場合のみ `websockets~=16.0` が必要 (`pyproject` の base 依存に含まれている)。

## API 概要

```python
from src.executor import (
    Algorithm,           # StrEnum: MARKET / PASSIVE / TWAP / MARKET_MAKE
    Intent,              # StrEnum: OPEN / CLOSE / SET_TARGET
    ExecutionStatus,     # StrEnum: RUNNING / FINALIZING / COMPLETED / ABORTED / FAILED
    ExecutorClient,      # async client
    ExecutorClientError, # raised on non-2xx
)
```

`ExecutorClient` は **async context manager** として使う必要がある:

```python
async with ExecutorClient("http://127.0.0.1:8085", timeout=10.0) as cli:
    health = await cli.health()
```

context manager 外で呼ぶと `RuntimeError`。

## メソッド

| メソッド | REST 対応 | 戻り値 |
|---|---|---|
| `await cli.health()` | GET /v1/health | `dict` |
| `await cli.positions()` | GET /v1/positions | `dict` |
| `await cli.book(symbol)` | GET /v1/book/{symbol} | `dict` |
| `await cli.start(...)` | POST /v1/exec | `StartResponse(exec_id, algorithm)` |
| `await cli.status(id)` | GET /v1/exec/{id} | `dict` |
| `await cli.cancel(id)` | POST /v1/exec/{id}/cancel | `dict` |
| `await cli.emergency_stop()` | POST /v1/emergency_stop | `dict` |
| `cli.stream(id)` | GET /v1/exec/{id}/ws | `AsyncIterator[dict]` |

## サンプル

### A. MARKET で 0.1 BTC を取って完了まで待つ

```python
import asyncio
from src.executor import ExecutorClient, Algorithm, Intent, ExecutionStatus

async def main():
    async with ExecutorClient("http://127.0.0.1:8085") as cli:
        resp = await cli.start(
            algorithm=Algorithm.MARKET,
            symbol="BTC",
            intent=Intent.OPEN,
            target_size="0.1",
            params={"max_slippage_bps": "20", "max_attempts": 3},
        )
        print(f"started exec_id={resp.exec_id}")

        # ポーリングで完了待ち
        while True:
            st = await cli.status(resp.exec_id)
            if st["status"] in (
                ExecutionStatus.COMPLETED,
                ExecutionStatus.ABORTED,
                ExecutionStatus.FAILED,
            ):
                print(f"final status: {st['status']}")
                print(st["report"])
                break
            await asyncio.sleep(0.2)

asyncio.run(main())
```

### B. WS で Progress を受けながら実行

```python
async def stream_example():
    async with ExecutorClient("http://127.0.0.1:8085") as cli:
        resp = await cli.start(
            algorithm=Algorithm.TWAP,
            symbol="BTC",
            intent=Intent.OPEN,
            target_size="1.0",
            params={"slice_count": 10, "total_duration_ms": 60000},
        )
        async for evt in cli.stream(resp.exec_id):
            t = evt.get("type")
            if t == "started":
                print("▶ started")
            elif t == "slice_filled":
                print(f"  slice {evt['slice']}: +{evt['sz']} @ {evt['px']} (cum={evt['cumulative_filled']})")
            elif t == "heartbeat":
                print(f"  hb: filled={evt['cumulative_filled']} remaining={evt['remaining']}")
            elif t == "completed":
                print(f"✓ completed avg_px={evt['avg_px']} fills={evt['n_fills']}")
                break
```

### C. キルスイッチ (operator id 付き)

```python
async def kill_all():
    async with ExecutorClient("http://127.0.0.1:8085") as cli:
        body = await cli.emergency_stop()
        print(f"aborted={body['aborted_executions']} cancelled={body['cancelled_orders']}")
```

> **注**: 現状の Python connector は `X-Operator-ID` をパラメータとして受け付けていない。
> 必要な場合は client を継承してヘッダ追加するか, httpx Client 直接利用が必要 (将来課題)。

## エラーハンドリング

```python
from src.executor import ExecutorClientError

async with ExecutorClient(url) as cli:
    try:
        await cli.start(algorithm="vwap", symbol="BTC", intent="open", target_size="0.1")
    except ExecutorClientError as e:
        if e.status == 400:
            print(f"bad request: {e.body}")  # {"code":"bad_request","message":"unknown algorithm: 'vwap'"}
        elif e.status == 404:
            print(f"not found: {e.body}")
        else:
            raise
```

`ExecutorClientError(status, body)` の `body` は dict (parse 成功時) or str。

## 型ヒント

すべて `from __future__ import annotations` 前提で `typing` 型ヒント完備。
`mypy src/executor` でチェック済み (CI で自動実行)。

`Decimal` を渡したい場合:

```python
from decimal import Decimal
await cli.start(..., target_size=Decimal("0.123456789012345"))  # str(...) でシリアライズされる
```

## StrEnum vs str

`Algorithm.MARKET` は `StrEnum` なので `"market"` と等価:

```python
Algorithm.MARKET == "market"  # True
Algorithm.MARKET.value         # "market"
```

文字列直書きでも OK:

```python
await cli.start(algorithm="passive_follow", ...)  # 別名も Rust 側で受け付ける
await cli.start(algorithm=Algorithm.PASSIVE, ...) # 推奨
```

## live e2e テスト

実バイナリを起動してエンドツーエンドで叩くテストが [`tests/test_executor_client_live.py`](../../../tests/test_executor_client_live.py) にあり,
`pytest -m live` で実行可。CI ではデフォルト skip。

```bash
cd executor && cargo build -p executor-server
cd .. && pytest tests/test_executor_client_live.py -m live -v
```

5 ケース: health, unknown algorithm 400, emergency_stop with no running, book 404,
start MARKET → cancel/poll status の chain。

## 制約 (Gemini PR-8 review より将来課題)

- **schema 中央化**: 現状 StrEnum を手書きで Rust serde に揃えている。将来は Pydantic
  + alias_generator で自動同期する案あり (今は採用せず, シンプル維持)
- **WS 再接続**: 現状再接続 / Lagged バックオフは実装なし。Python 側で wrap が必要

## 関連

- [REST API](../api/rest.md)
- [WebSocket API](../api/websocket.md)
- [executor-cli](../cli.md)
