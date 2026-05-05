# PR-B2a: place_orders / cancel_orders 実装 + mock backend 検証 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-b2a-place-cancel-mock` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage B §6
**前提コード**: PR-B1 merged (`develop@564f814`)

## 1. 目的

`RealHlClient::place_orders` / `cancel_orders` の実装を完成させる. 具体的には:

1. `OrderIntent` / `CancelIntent` (executor-core) を HL `/exchange` 用の wire 構造へ変換
2. `Eip712AgentSigner` で署名 (PR-B1 完成済 signer をそのまま利用)
3. POST `/exchange` で送信
4. response を parse して `OrderResponse` (cloid, oid, status, error) に分解

加えて `Signer::sign_l1` trait に `vault: Option<&Address>` を追加し, vault cross-check 2 件
(`dummy_with_vault_*`) を有効化して **10/10 cross-check** を達成する.

検証は **mockito ベースの mock backend** で完結させる. 実 testnet smoke は **PR-B2b** (別 spec) で扱う.

## 2. 非目的

- 実 testnet / 実 mainnet POST 検証 (PR-B2b)
- testnet agent wallet 発行手順 / faucet 入手 (PR-B2b)
- r/s padding 問題の HL 実環境確認 (PR-B2b)
- WS subscriber, Auth レイヤ, MARKET_MAKE 等 (Phase 3.5 別 step)
- mainnet 極小発注 (PR-C)

## 3. 制約と前提

### 3.1 PR-B1 完成済の利用

| 既存資産 | 使い方 |
|---|---|
| `Eip712AgentSigner::from_secret(SecretString, is_mainnet)` | そのまま. テストでは known PK を渡す |
| `pack_action`, `action_hash`, `build_agent`, `l1_domain` | place/cancel action の hash 計算に再利用 |
| `OrderAction`, `OrderWire`, `OrderTypeWire`, `LimitTif` | place_orders で使用 |
| `signing_cross_check.rs` 既存 8 件 | vault 2 件追加して 10/10 |

### 3.2 既存 caller の影響範囲

`HlClient::sign_l1` を直接呼び出す call site (`grep` で確認, 2026-05-05 時点):
- `executor-hl` 内部のみ (test 含む)
- `executor-algo`, `executor-server`, `executor-cli` には直接 call なし

→ trait 変更による downstream 影響は executor-hl 内に限定.

### 3.3 HL python-sdk 0.23.0 の wire format (PR-B1 調査済)

#### POST `/exchange` request body

```json
{
  "action": { ... },
  "nonce": 1234567890123,
  "signature": { "r": "0x...", "s": "0x...", "v": 27 },
  "vaultAddress": "0x..." | null,
  "expiresAfter": 1234567890123 | null
}
```

`vaultAddress` と `expiresAfter` は省略時 omit (`null` を出さない場合もあるので `Option` + `skip_serializing_if`).

#### Order action body

```json
{
  "type": "order",
  "orders": [{
    "a": <asset_index_u32>,
    "b": <is_buy_bool>,
    "p": "<limit_px_str>",
    "s": "<sz_str>",
    "r": <reduce_only_bool>,
    "t": {"limit": {"tif": "Gtc"|"Alo"|"Ioc"}},
    "c": "0x...32hex"   // optional, omit if None
  }],
  "grouping": "na"
}
```

#### CancelByCloid action body

```json
{
  "type": "cancelByCloid",
  "cancels": [{
    "asset": <asset_index_u32>,
    "cloid": "0x...32hex"
  }]
}
```

注意: cancel ペイロードのキーは `asset` (full word), order の `a` (省略形) と異なる.

#### Response success

```json
{
  "status": "ok",
  "response": {
    "type": "order",
    "data": {
      "statuses": [
        {"resting": {"oid": 12345}}
        |
        {"filled": {"totalSz": "0.1", "avgPx": "100.5", "oid": 12345}}
      ]
    }
  }
}
```

#### Response error (per-order)

```json
{
  "status": "ok",
  "response": {
    "type": "order",
    "data": {
      "statuses": [{"error": "MinTradeNtl"}]
    }
  }
}
```

#### Response top-level error

```json
{ "status": "err", "response": "Some error message" }
```

(top-level err は HTTP 200 で返ることが多い, status を必ず確認)

### 3.4 asset index 解決

HL の `meta` endpoint (PR-A 実装済) から `WireMeta.universe[].name` 経由で symbol → index を解決.
PR-B2a では symbol-to-index lookup を `RealHlClient` 内に持たせず, **テスト時はハードコード** とし, 将来 `executor-server` 起動時に `fetch_meta()` から作る前提.

→ PR-B2a の `place_orders` 実装は **`OrderIntent` に asset_index field がある前提** で書く. 既存 `OrderIntent` を確認:

```bash
grep -A20 "pub struct OrderIntent" executor/crates/executor-core/src/intent.rs
```

asset_index がない場合は PR-B2a で追加する (executor-core に低リスクな field 追加, downstream は default値で動く).

### 3.5 cancel 戦略 (確定)

**`cancelByCloid` のみ実装.** `CancelIntent.by_oid: Some(_)` で渡されたら `HlError::ActionFormat` で reject.
理由:
- PR-A の `OrderResponse.cloid` 中心設計と整合
- batch_sender が cloid 中心
- HL python-sdk の運用パターン
- `by_oid` 経路は emergency_stop 等で将来必要だが PR-B2a スコープ外

### 3.6 r/s padding (HL 実環境確認は PR-B2b)

PR-B1 で発覚した leading-zero 問題: HL python-sdk は `eth_utils.to_hex(int)` で leading 0 を strip,
Rust は常に 64 chars padded.

PR-B2a では **Rust 出力 (padded) のまま `/exchange` に投げる前提** で実装する.
mock backend は HTTP body を assert しない (statuses だけ assert) ので mock test では問題化しない.
実 HL がどちらを accept するかは PR-B2b testnet smoke で確定する.

## 4. 技術スタック

| 項目 | 採用 | 備考 |
|---|---|---|
| HTTP mock | `mockito = "1.7.2"` | dev-deps 追加 (crates.io 2026-05-05 latest). `Server::new_async` で URL 取得 → `HlConfig` 差し替え |
| 既存 alloy / rmp-serde / hex | PR-B1 で導入済 | 追加なし |
| async test | 既存 `tokio = { features = ["macros", "rt-multi-thread", "test-util"] }` | 追加なし |

`mockito` を `[workspace.dependencies]` に追加し, `executor-hl/Cargo.toml` の `[dev-dependencies]` で参照.

## 5. 実装設計

### 5.1 ファイル構成

| パス | 役割 | アクション |
|---|---|---|
| `executor/Cargo.toml` | `[workspace.dependencies]` に `mockito = "1.7.2"` | 修正 |
| `executor/crates/executor-hl/Cargo.toml` | `[dev-dependencies]` に `mockito = { workspace = true }` | 修正 |
| `executor/crates/executor-hl/src/signer.rs` | `Signer::sign_l1` に `vault: Option<&Address>` 引数追加. `MockSigner` / `Eip712AgentSigner` 両方更新. `Eip712AgentSigner::dispatch_and_hash` に vault thread | 修正 |
| `executor/crates/executor-hl/src/eip712.rs` | `CancelByCloidAction` / `CancelByCloidWire` struct 追加. `OrderRequest → OrderWire` ヘルパー追加 (`order_intent_to_wire`) | 修正 |
| `executor/crates/executor-hl/src/hl_client.rs` | `RealHlClient::place_orders` / `cancel_orders` を本実装. `OrderResponse` parse ロジック新設. `_sig` 廃止 | 修正 |
| `executor/crates/executor-hl/tests/signing_cross_check.rs` | vault 2 件の skip を撤去, vault 引数を渡す. 10/10 pass | 修正 |
| `executor/crates/executor-hl/tests/place_cancel_mock.rs` | NEW. mockito で `/exchange` 模擬, 各種ケース (success / per-order error / top-level err / response shape variants) | **新規** |
| `executor/crates/executor-core/src/intent.rs` | `OrderIntent.asset` field 追加 (u32) — 既存があれば skip | 修正 (条件付) |
| `docs/HANDOFF-2026-05-04.md` | PR-B2a 完了行追加 | 修正 |

### 5.2 trait 拡張

```rust
#[async_trait]
pub trait Signer: Send + Sync {
    fn address(&self) -> Address;

    /// Sign an L1 action with a specific nonce.
    /// `vault` allows trading on behalf of a vault/subaccount; pass `None`
    /// for direct master/agent action.
    async fn sign_l1(
        &self,
        action: &Action,
        nonce: u64,
        vault: Option<&Address>,
    ) -> Result<Signature, HlError>;
}
```

`MockSigner` 更新 (vault 無視, 既存 deterministic 動作維持):
```rust
async fn sign_l1(
    &self,
    _action: &Action,
    nonce: u64,
    _vault: Option<&Address>,
) -> Result<Signature, HlError> {
    // 既存 dummy 出力
    Ok(Signature {
        r: format!("0x{:064x}", nonce),
        s: format!("0x{:064x}", nonce.wrapping_add(1)),
        v: 27,
    })
}
```

`Eip712AgentSigner::sign_l1` 更新:
```rust
async fn sign_l1(
    &self,
    action: &Action,
    nonce: u64,
    vault: Option<&Address>,
) -> Result<Signature, HlError> {
    // 内部で executor_core::types::Address (String) を alloy::primitives::Address (20-byte) へ変換
    let vault_alloy = vault
        .map(|a| {
            a.as_str().parse::<AlloyAddress>()
                .map_err(|e| HlError::ActionFormat(format!("vault address parse: {e}")))
        })
        .transpose()?;
    let hash = dispatch_and_hash(action, nonce, vault_alloy.as_ref())?;
    // ... 以降は PR-B1 と同じ ...
}
```

### 5.3 cancel wire structs (eip712.rs に追加)

```rust
/// `{"type": "cancelByCloid", "cancels": [...]}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidAction {
    #[serde(rename = "type")]
    pub action_type: String,    // "cancelByCloid"
    pub cancels: Vec<CancelByCloidWire>,
}

/// One cancel wire item. Field order: asset, cloid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidWire {
    pub asset: u32,
    pub cloid: String,          // 0x + 32 hex
}
```

`dispatch_and_hash` に追加:
```rust
"cancelByCloid" => {
    let typed = CancelByCloidAction::deserialize(action)
        .map_err(|e| HlError::ActionFormat(format!("cancelByCloid decode: {e}")))?;
    action_hash(&typed, nonce, vault, None)
        .map_err(|e| HlError::ActionFormat(format!("cancelByCloid msgpack: {e}")))
}
```

### 5.4 `OrderIntent → OrderWire` 変換 (eip712.rs に追加)

```rust
use executor_core::intent::OrderIntent;
use executor_core::types::{Side, Tif};

/// Convert an `OrderIntent` (executor-core domain type) into the HL wire
/// shape `OrderWire`.
///
/// `asset` is the HL universe index for the symbol; resolution from
/// `Symbol` → `asset` is the caller's responsibility (typically via
/// `fetch_meta()` cache at server startup).
pub fn order_intent_to_wire(intent: &OrderIntent, asset: u32) -> OrderWire {
    OrderWire {
        a: asset,
        b: matches!(intent.side, Side::Long),
        p: format!("{}", intent.px),    // rust_decimal Display = canonical decimal string
        s: format!("{}", intent.sz),
        r: intent.reduce_only,
        t: OrderTypeWire {
            limit: LimitTif {
                tif: match intent.tif {
                    Tif::Alo => "Alo".into(),
                    Tif::Ioc => "Ioc".into(),
                    Tif::Gtc => "Gtc".into(),
                },
            },
        },
        c: Some(format!("{}", intent.cloid)),  // PR-A の Cloid は Display で 0x + 32 hex
    }
}
```

注: `format!("{}", decimal)` は `rust_decimal` の `Display` impl で **canonical** decimal string (trailing zero なし, scientific notation なし) を出す.
HL python-sdk の `float_to_wire(x)` は `f"{x:.8f}"` → `Decimal` normalize → `f"{normalized:f}"` で
同じ canonical 形になる. 差異が出たら test で検出する.

### 5.5 `RealHlClient::place_orders` 実装

```rust
async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
    if orders.is_empty() {
        return Ok(Vec::new());
    }

    // rate limit (HL exchange weight = 1 + floor(batch_len/40))
    let weight = 1 + (orders.len() as u32 / 40);
    let _wait = self.rate_limiter.acquire(weight).await;

    // 1. asset index 解決 — PR-B2a では caller が事前に decorate する想定で
    //    OrderIntent.asset field を要求. Symbol→asset の動的解決は別 PR.
    let order_wires: Vec<OrderWire> = orders.iter()
        .map(|o| order_intent_to_wire(o, o.asset))
        .collect();

    let action = OrderAction {
        action_type: "order".into(),
        orders: order_wires,
        grouping: "na".into(),
    };
    let action_value = serde_json::to_value(&action)
        .map_err(|e| HlError::ActionFormat(format!("order serialize: {e}")))?;

    // 2. nonce
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();

    // 3. sign (vault は PR-B2a では None 固定 — vault 対応は PR-B2b で trait 拡張済み引数経由)
    let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

    // 4. POST body
    let body = serde_json::json!({
        "action": action,
        "nonce": nonce,
        "signature": sig,
        "vaultAddress": null,
    });

    // 5. POST /exchange
    let resp_text = self.post_exchange(&body).await?;

    // 6. response parse
    parse_exchange_response(&resp_text, orders)
}
```

`post_exchange` ヘルパー:
```rust
impl RealHlClient {
    async fn post_exchange(&self, body: &serde_json::Value) -> Result<String, HlError> {
        let resp = self.http
            .post(&self.config.exchange_url)
            .json(body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(HlError::Network(format!("HTTP {status}: {text}")));
        }
        Ok(text)
    }
}
```

`parse_exchange_response`:
```rust
fn parse_exchange_response(
    text: &str,
    orders: &[OrderIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse json: {e}")))?;

    // top-level err?
    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v.get("response").and_then(|r| r.as_str()).unwrap_or("(no msg)");
        return Err(HlError::Exchange {
            code: Some("top_level_err".into()),
            message: msg.into(),
        });
    }

    let statuses = v
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| HlError::InvalidResponse("statuses missing".into()))?;

    if statuses.len() != orders.len() {
        return Err(HlError::InvalidResponse(format!(
            "statuses len {} != orders len {}",
            statuses.len(), orders.len()
        )));
    }

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(orders.iter()) {
        let cloid = intent.cloid;
        // success cases
        if let Some(resting) = status.get("resting") {
            let oid = resting.get("oid").and_then(|o| o.as_u64())
                .ok_or_else(|| HlError::InvalidResponse("resting.oid missing".into()))?;
            out.push(OrderResponse {
                cloid,
                oid: Some(OrderId(oid)),
                status: "resting".into(),
                error: None,
            });
        } else if let Some(filled) = status.get("filled") {
            let oid = filled.get("oid").and_then(|o| o.as_u64())
                .ok_or_else(|| HlError::InvalidResponse("filled.oid missing".into()))?;
            out.push(OrderResponse {
                cloid,
                oid: Some(OrderId(oid)),
                status: "filled".into(),
                error: None,
            });
        } else if let Some(err) = status.get("error") {
            let msg = err.as_str().unwrap_or("(no msg)");
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "error".into(),
                error: Some(msg.into()),
            });
        } else {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "unknown".into(),
                error: Some(format!("unknown status shape: {status}")),
            });
        }
    }
    Ok(out)
}
```

### 5.6 `RealHlClient::cancel_orders` 実装

```rust
async fn cancel_orders(&self, cancels: &[CancelIntent]) -> Result<Vec<OrderResponse>, HlError> {
    if cancels.is_empty() {
        return Ok(Vec::new());
    }

    let weight = 1 + (cancels.len() as u32 / 40);
    let _wait = self.rate_limiter.acquire(weight).await;

    // by_cloid のみ受理. by_oid は明示 reject.
    let cancel_wires: Result<Vec<CancelByCloidWire>, HlError> = cancels.iter()
        .map(|c| {
            if c.by_oid.is_some() {
                return Err(HlError::ActionFormat(
                    "by_oid cancel not supported in PR-B2a; use by_cloid".into()
                ));
            }
            let cloid = c.by_cloid.ok_or_else(|| HlError::ActionFormat(
                "CancelIntent missing both by_cloid and by_oid".into()
            ))?;
            Ok(CancelByCloidWire {
                asset: c.asset,    // CancelIntent も asset field を持つ前提
                cloid: format!("{}", cloid),
            })
        })
        .collect();
    let cancel_wires = cancel_wires?;

    let action = CancelByCloidAction {
        action_type: "cancelByCloid".into(),
        cancels: cancel_wires,
    };
    let action_value = serde_json::to_value(&action)
        .map_err(|e| HlError::ActionFormat(format!("cancel serialize: {e}")))?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();

    let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

    let body = serde_json::json!({
        "action": action,
        "nonce": nonce,
        "signature": sig,
        "vaultAddress": null,
    });

    let resp_text = self.post_exchange(&body).await?;

    // cancel response: data.statuses は ["success" | {error: "..."}] (string で来る場合あり)
    parse_cancel_response(&resp_text, cancels)
}

fn parse_cancel_response(
    text: &str,
    cancels: &[CancelIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse json: {e}")))?;

    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v.get("response").and_then(|r| r.as_str()).unwrap_or("(no msg)");
        return Err(HlError::Exchange {
            code: Some("top_level_err".into()),
            message: msg.into(),
        });
    }

    let statuses = v
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| HlError::InvalidResponse("cancel statuses missing".into()))?;

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(cancels.iter()) {
        let cloid = intent.by_cloid.unwrap_or_default();
        // HL は cancel success を文字列 "success" で返す
        if status.as_str() == Some("success") {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "cancelled".into(),
                error: None,
            });
        } else if let Some(err) = status.get("error") {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "error".into(),
                error: Some(err.as_str().unwrap_or("(no msg)").into()),
            });
        } else {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "unknown".into(),
                error: Some(format!("unknown cancel status shape: {status}")),
            });
        }
    }
    Ok(out)
}
```

### 5.7 mock backend test (place_cancel_mock.rs)

```rust
//! Mock-backend integration tests for RealHlClient::place_orders/cancel_orders.
//!
//! Uses mockito to mock HL /exchange responses. No real network, no PK
//! beyond the well-known test PK. Real testnet smoke is in PR-B2b.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::cloid::Cloid;
use executor_core::intent::{CancelIntent, OrderIntent};
use executor_core::types::{Side, Tif};
use executor_core::symbol::Symbol;
use executor_hl::hl_client::{HlClient, HlConfig, RealHlClient};
use executor_hl::signer::Eip712AgentSigner;
use rust_decimal_macros::dec;
use secrecy::SecretString;
use std::sync::Arc;

const TEST_PK: &str =
    "0x0123456789012345678901234567890123456789012345678901234567890123";

fn make_client(server_url: &str) -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap()
    );
    let config = HlConfig {
        info_url: format!("{server_url}/info"),
        exchange_url: format!("{server_url}/exchange"),
        ws_url: "ws://unused".into(),
    };
    RealHlClient::new(config, signer)
}

fn make_order_intent() -> OrderIntent {
    OrderIntent {
        cloid: Cloid::new(),
        symbol: Symbol::new("ETH"),
        asset: 1,                // ETH on default dex
        side: Side::Long,
        px: dec!(2000),
        sz: dec!(0.001),
        tif: Tif::Alo,
        reduce_only: false,
    }
}

#[tokio::test]
async fn place_orders_resting_response_parses_to_oid() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("POST", "/exchange")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":12345}}]}}}"#)
        .create_async()
        .await;

    let client = make_client(&server.url());
    let resp = client.place_orders(&[make_order_intent()]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "resting");
    assert_eq!(resp[0].oid.as_ref().map(|o| o.0), Some(12345));
    assert!(resp[0].error.is_none());
}

#[tokio::test]
async fn place_orders_filled_response_parses_to_oid_and_filled_status() { /* ... */ }

#[tokio::test]
async fn place_orders_per_order_error_keeps_cloid_and_attaches_error() { /* ... */ }

#[tokio::test]
async fn place_orders_top_level_err_returns_hl_error_exchange() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("POST", "/exchange")
        .with_status(200)
        .with_body(r#"{"status":"err","response":"Insufficient margin"}"#)
        .create_async()
        .await;

    let client = make_client(&server.url());
    let err = client.place_orders(&[make_order_intent()]).await.unwrap_err();
    match err {
        executor_hl::errors::HlError::Exchange { code, message } => {
            assert_eq!(code.as_deref(), Some("top_level_err"));
            assert!(message.contains("Insufficient margin"));
        }
        other => panic!("expected Exchange err, got {other:?}"),
    }
}

#[tokio::test]
async fn place_orders_empty_returns_empty() {
    let server = mockito::Server::new_async().await;
    let client = make_client(&server.url());
    let resp = client.place_orders(&[]).await.unwrap();
    assert!(resp.is_empty());
}

#[tokio::test]
async fn cancel_orders_success_string_response() { /* ... */ }

#[tokio::test]
async fn cancel_orders_by_oid_returns_action_format_error() { /* ... */ }
```

最低 7 件 (place 5 + cancel 2). 各テストは:
1. mockito server 立ち上げ
2. 期待 response 設定
3. RealHlClient 経由で呼び出し
4. パース結果 assert

mockito の req body assert は **最小限**にする (signature 値が nonce 依存で時間によって変わるため、構造のみ verify):
- `match_body(mockito::Matcher::PartialJson(json!({"action": {"type": "order"}})))` 程度

### 5.8 OrderIntent の asset field 確認

実装前に `executor/crates/executor-core/src/intent.rs` を確認.
- asset field 既存 → そのまま使う
- 未存在 → PR-B2a で `pub asset: u32` 追加 (既存 caller は default value 0 で動作不能なので serde default ではなく **必須 field** で追加)

→ `executor-algo` 等の既存 caller に `OrderIntent { asset: ... }` 構築箇所が複数ある場合は影響大.
追加前に `grep -rn "OrderIntent {" executor/crates/` で hit 数を測ってから判断.

### 5.9 受け入れ基準

- [ ] `cargo test -p executor-hl --test signing_cross_check` で **10/10** 一致 (vault 2 件の skip 撤去)
- [ ] `cargo test -p executor-hl --test place_cancel_mock` で 7+ 件 pass
- [ ] `cargo test --workspace` で全 pass (133 baseline + 新規 + cross-check 2 = 142 程度)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] CI green
- [ ] Gemini deep review 取得 → MUST-FIX 全対応

## 6. 既知の落とし穴

### 6.1 `OrderIntent.asset` の追加責務

PR-A 時点で field がない場合, 既存 caller (executor-algo の market.rs / market_make.rs / passive_follow.rs / twap.rs / batch_sender.rs 等) が **ほぼ全てコンパイル失敗** する. 影響範囲を確認してから追加方針を決める. 選択肢:
- A: required field で追加 → 全 caller を 1 commit で fix
- B: `Option<u32>` で追加 → caller は変えずに place_orders 内で `unwrap_or` でハンドル, 不明なら error

→ 確認後に決定 (実装時に grep で hit 数チェック).

### 6.2 cancel response の `"success"` 文字列

HL は cancel success を **文字列** `"success"` で返す (object ではない). place response の `{"resting":{...}}` と shape が異なるため, parser を別関数 (`parse_cancel_response`) にする.

### 6.3 mockito の URL trailing slash

`server.url()` は `http://127.0.0.1:XXXXX` (slash なし). `format!("{server_url}/exchange")` で path 結合するときに double slash にならないよう注意.

### 6.4 nonce が time-based なので req body fixed assert 不能

`SystemTime::now()` で nonce 生成するため, mockito の `match_body` で完全一致 assert は不可. PartialJson matcher で `action` だけ assert する.

### 6.5 vault parse 失敗の error variant

vault address parse 失敗 → `HlError::ActionFormat`. `InvalidConfig` ではない (sign 時の動的データなので).

### 6.6 r/s padding

PR-B1 で発覚した leading-zero 問題は **PR-B2a の mock test では非問題** (mock body assert しない). PR-B2b testnet で実 HL の accept 確認.

## 7. リスクと PR-B2b への引き継ぎ

| リスク | 影響 | PR-B2a 対応 | PR-B2b で確認 |
|---|---|---|---|
| r/s padded を HL が拒否 | place 失敗 | mock では検出不能 | testnet で実投入確認 |
| asset index lookup 動的化 | symbol→index 解決責任 | OrderIntent.asset field で外部解決を要求 | testnet 起動時に fetch_meta cache |
| vault 引数の実用性 | 主要利用なし | trait 拡張のみ, vault=None で固定 | subaccount trade 実証は別 PR |
| nonce 衝突 (高頻度発注) | 重複 nonce で reject | mock では問題化せず | testnet で実機検証 (PR-B2b は単発往復のみ) |

## 8. 関連リンク

- 親 spec: [`2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md`](2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md)
- PR-B1 spec: [`2026-05-05-pr-b1-eip712-signer-design.md`](2026-05-05-pr-b1-eip712-signer-design.md)
- PR-B1 plan: `docs/superpowers/plans/2026-05-05-pr-b1-eip712-signer-plan.md`
- HL exchange endpoint: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>
- HL python-sdk exchange.py: <https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/exchange.py>
- mockito: <https://docs.rs/mockito/latest/mockito/>
