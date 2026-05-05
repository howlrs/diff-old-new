# PR-C4: multi-symbol live test + Python e2e + operator_id (Phase 3.5 final) 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-c4-multi-symbol-and-e2e` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C
**前提コード**: PR-C3 merged (`develop@e9e0d15`)
**Gemini deep review**: 2026-05-05, `review_log` (5/6 同意 + Q3 で Claude を flip)

## 1. 目的

Phase 3.5 を完成させるため, 以下 3 件を 1 PR で:

1. **multi-symbol cancel live test** (testnet + 1-shot agent wallet) — emergency_stop で multi-cancel 経路を実機検証
2. **Python connector の `X-Operator-ID` 全 POST 対応** (HANDOFF §5.5 deferred 解消)
3. **Python e2e test を CI に組み込む** (mock 限定. `slow` のみ marker, `live` は除外)

これにより:
- "executor-server を Python から駆動して全 round trip 通る" 証拠が CI で常時担保される
- mainnet 投入時の audit log で誰が start_exec したか追跡可能
- Phase 3.5 完成宣言 → Phase 4 (WS subscriber 等) へ移行

## 2. 非目的

- `by_oid` cancel 経路の RealHlClient 実装 (PR-D 系で別途)
- mainnet での multi-symbol live test (testnet で十分)
- WS 経由の即時 baseline-diff 検知 (PR-D 系)

## 3. Gemini deep flip / 採用論点

| Q | Claude 案 | Gemini 推奨 | 採用 |
|---|---|---|---|
| Q1+Q7 | testnet 新規 wallet | testnet + 1-shot key | testnet |
| Q2 | Step A only | Step A only (責務分離) | Step A |
| **Q3+Q8** | **CI 除外** | **CI 含める (mock 限定)** | **CI に含める** |
| Q4 | 全 POST に operator_id | 同意 | 全 POST |
| Q5 | by_oid 含めない | 含めない | 含めない |
| Q6 | (a)(b)(c) で完了 | OK | (a)(b)(c) |

## 4. アーキテクチャ

### 4.1 Python connector の operator_id 追加

```python
class ExecutorClient:
    def __init__(self, base_url: str, operator_id: str | None = None, timeout: float = 10.0):
        self._base_url = base_url.rstrip("/")
        self._operator_id = operator_id
        self._timeout = timeout
        self._client: httpx.AsyncClient | None = None

    async def _post(self, path: str, payload: dict) -> dict:
        headers = {}
        if self._operator_id:
            headers["X-Operator-ID"] = self._operator_id
        resp = await self._http.post(f"{self._base_url}{path}", json=payload, headers=headers)
        return self._unwrap(resp)
```

注: `_get` は header を付けない (audit は POST のみ. GET は read-only で副作用なし).

### 4.2 Server 側 audit log

`routes::start_exec` および `routes::cancel_exec` で `X-Operator-ID` を log 出力.
既存 `emergency_stop` は実装済 (PR-7 から). 新規:

```rust
// routes::start_exec の先頭に追加
let operator = headers.get("x-operator-id").and_then(|v| v.to_str().ok()).unwrap_or("unknown");
tracing::info!(operator, algorithm = %req.algorithm, symbol = %req.symbol, "start_exec");
```

`HeaderMap` を引数で受け取る必要あり. axum extractor で簡単.

### 4.3 multi-symbol live test (testnet, Step A)

`executor/crates/executor-hl/tests/live_emergency_stop_multi_testnet.rs` (新規):

```rust
#![cfg(feature = "live")]

const ENV_TESTNET_AGENT_PK: &str = "HL_TESTNET_AGENT_PK";
const ENV_TESTNET_MASTER: &str = "HL_TESTNET_MASTER";

/// Multi-symbol cancel via cancel_orders(&[2 件]). testnet only — fails fast
/// if HL_TESTNET_AGENT_PK is unset.
#[tokio::test]
async fn live_testnet_multi_cancel_two_symbols() {
    // 1. testnet client + signer (HL_TESTNET_AGENT_PK 必須)
    // 2. ETH + BTC 2 件をそれぞれ別 cloid で place (ALO post-only, $11 notional each)
    // 3. cancel_orders(&[c1, c2]) → 両方 cancelled で返ること
    // 4. existing positions が変化しないこと
}
```

CI の `--features live` は **付けない**. Claude session でも実行されない. ユーザー実行用 placeholder.

### 4.4 Python e2e test を CI に組み込む

既存 `tests/test_executor_client_live.py` から `@pytest.mark.live` を外す. `@pytest.mark.slow` のみ残す.
CI marker filter を `-m "not live"` に変更 (`slow` は通す).

ただし binary が前提なので, CI workflow に **`cargo build --release -p executor-server` を pytest 前に実行**.

CI 時間試算:
- `cargo build --release -p executor-server`: ~30s (cold) / ~5s (cached)
- `pytest -m "not live"`: 既存 +5s 程度
- 全体 +30s 程度. CI 時間 < 3 分は維持.

ローカル `scripts/check_ci_local.sh` も同期: `pytest -m "not live"` に変更 + 事前に `cargo build --release -p executor-server`.

### 4.5 既存 test_executor_client_live.py の改修

```diff
- @pytest.mark.live
  @pytest.mark.slow
  @pytest.mark.asyncio
  async def test_e2e_health_and_algorithms(running_server: str) -> None:
```

新規テスト:
```python
@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_emergency_stop_with_operator_id(running_server: str) -> None:
    async with ExecutorClient(running_server, operator_id="alice@desk") as cli:
        body = await cli.emergency_stop()
    assert body["aborted_executions"] == 0

@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_idempotent_emergency_stop(running_server: str) -> None:
    """PR-C3 idempotency check via Python connector."""
    async with ExecutorClient(running_server) as cli:
        a = await cli.emergency_stop()
        b = await cli.emergency_stop()
    assert b["aborted_executions"] == 0
    assert b["cancelled_orders"] == 0

@pytest.mark.slow
@pytest.mark.asyncio
async def test_e2e_503_after_emergency_stop(running_server: str) -> None:
    """PR-C3 503 check via Python connector."""
    async with ExecutorClient(running_server) as cli:
        await cli.emergency_stop()
        with pytest.raises(Exception, match="HTTP 503"):
            await cli.start(algorithm="market", symbol="BTC", intent="open", target_size="0.01")
```

注: `running_server` fixture は scope="module" だが PR-C3 emergency_stop は state を mutate するので, テスト間で副作用が出る. **fixture を function scope に変更** が必要 — もしくは新規 fixture `fresh_server` (per-test).

## 5. テスト計画

### 5.1 既存 unit tests (executor-server, hl-client)
影響なし. operator_id は header だけなので server 側で受け取り log するだけ.

### 5.2 新規 Python tests (CI 含む)
- `test_executor_client.py::test_post_includes_operator_id_when_set` (mock transport)
- `test_executor_client.py::test_post_omits_operator_id_when_none` (mock transport)
- `test_executor_client_live.py::test_e2e_emergency_stop_with_operator_id` (live binary)
- `test_executor_client_live.py::test_e2e_idempotent_emergency_stop` (live binary)
- `test_executor_client_live.py::test_e2e_503_after_emergency_stop` (live binary)

### 5.3 新規 Rust live test (CI 除外)
- `live_emergency_stop_multi_testnet.rs::live_testnet_multi_cancel_two_symbols`
- `#[cfg(feature = "live")]` で CI から除外
- ユーザー実行: `source scripts/load-env-testnet.sh && cargo test --features live live_testnet_multi_cancel`

注: testnet 用 `load-env-testnet.sh` は新規. `HL_TESTNET_AGENT_PK` を pass-store から export する想定.
鍵管理は `~/.password-store/diff-old-new/hl-testnet/agent-pk.gpg` で別保管 (mainnet 鍵と分離).

ユーザーが手元で testnet wallet を発行 → pass-store に保存 → `load-env-testnet.sh` を起動の流れは別 README で.
PR-C4 では `load-env-testnet.sh` の **template** だけ提供 (PK 値は user が埋める).

## 6. 実装順序

1. `src/executor/client.py::ExecutorClient` に `operator_id` 追加 (init + `_post`)
2. `tests/test_executor_client.py` で operator_id mock test 2 件
3. `routes.rs::start_exec` で `X-Operator-ID` log 出力 (HeaderMap extractor)
4. `tests/test_executor_client_live.py` で `@pytest.mark.live` 外す + 新 3 テスト
5. running_server fixture を function scope に変更 (state mutation 対策)
6. CI workflow / `scripts/check_ci_local.sh` で `pytest -m "not live"` + `cargo build --release` 事前
7. `executor-hl/tests/live_emergency_stop_multi_testnet.rs` 新規 (`#[cfg(feature = "live")]`)
8. `scripts/load-env-testnet.sh` template 新規
9. `docs/HANDOFF-2026-05-05.md` で Phase 3.5 完成宣言
10. fmt / clippy / test / CI script
11. commit / push / PR (--base develop)

## 7. リスクとフォールバック

| リスク | 影響 | 対策 |
|---|---|---|
| running_server fixture の module scope で state shared | テスト間で fall-through | function scope に変更 |
| CI で binary build が遅い | CI 時間 +30s | `cargo build --release` cache (Github Actions) で吸収 |
| Python e2e の port competition | flaky | `_free_port()` を使う (既存) |
| testnet wallet が別人の鍵漏洩 | testnet で実損失なし | 1-shot key + pass-store 管理. PR-C4 では template のみ提供 |
| HL min notional $10 in testnet | 不明 (testnet は仕様変動) | live test 内で MIN_NOTIONAL_USD assertion で fail-fast |

## 8. 受入条件

- [ ] `cargo build --workspace` clean
- [ ] `cargo test --workspace` 全 pass (173 + ~? 件 = 同程度)
- [ ] Python `pytest -m "not live"` で e2e tests も実行され全 pass
- [ ] `scripts/check_ci_local.sh` green (新ロジックで)
- [ ] `cargo fmt --check` / `cargo clippy -D warnings` clean
- [ ] CI green (mock e2e 込み)
- [ ] Phase 3.5 完成宣言が HANDOFF doc に書かれる

## 9. Phase 3.5 完成判定

PR-C4 merge をもって Phase 3.5 完成. 以下が全て揃った状態:

- ✅ HL mainnet read-only parser (PR-A)
- ✅ EIP-712 signer (PR-B1, byte-identical 10/10)
- ✅ RealHlClient::place_orders/cancel_orders + mainnet 1-round-trip (PR-B2a/b)
- ✅ MetaCache + executor-server real mode 切替 (PR-C1)
- ✅ symbol allowlist + size cap 2 段防御 (PR-C2)
- ✅ baseline-diff guard + idempotent emergency_stop (PR-C3)
- ✅ multi-symbol cancel live test placeholder + Python e2e CI 組み込み + operator_id (PR-C4)

未完: WS subscriber 本実装 (Phase 4 PR-D 系), 片肺リスク (PR-D), by_oid cancel (PR-D)

PR-C4 後の executor-server は **production 投入可能状態**.
