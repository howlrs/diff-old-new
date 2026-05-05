# PR-B1: Eip712AgentSigner 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-b1-eip712-signer` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage B §6
**前提コード**: `executor-hl/src/signer.rs`, PR-A merged (`develop@2051d3d`)

## 1. 目的

Hyperliquid L1 action (place / cancel / scheduleCancel など `/exchange` POST 系) を
EIP-712 で署名する `Eip712AgentSigner` を `executor-hl` crate に追加する.
HL python-sdk 0.23.0 (master 系) との **既知ベクタ完全一致** を必達.

`/exchange` への実 POST は本 PR の範囲外 (PR-B2 で扱う). 本 PR は **署名アルゴリズムの正しさを単独で検証** するスコープ.

## 2. 非目的

- `RealHlClient::place_orders` / `cancel_orders` の HTTP 実装 (PR-B2)
- `/exchange` payload schema (PR-B2)
- testnet smoke (PR-B2)
- User-signed action (UsdSend, Withdraw, SubAccountTransfer) — PR-A 後続の用途で必要になったら別 PR
- mainnet PK 投入 / 実発注 (PR-B/C)

## 3. 仕様根拠

### 3.1 HL python-sdk signing.py (master, 2026-05-05 確認)

参照: <https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/utils/signing.py>

#### action_hash アルゴリズム

```python
def action_hash(action, vault_address, nonce, expires_after):
    data = msgpack.packb(action)
    data += nonce.to_bytes(8, "big")
    if vault_address is None:
        data += b"\x00"
    else:
        data += b"\x01" + address_to_bytes(vault_address)
    if expires_after is not None:
        data += b"\x00" + expires_after.to_bytes(8, "big")
    return keccak(data)
```

#### EIP-712 Domain (L1 action)

```
name: "Exchange"
version: "1"
chainId: 1337                 # mainnet/testnet 共通の固定値
verifyingContract: 0x0000000000000000000000000000000000000000
```

#### PrimaryType / Types

```
PrimaryType: "Agent"
Agent {
    source: string,
    connectionId: bytes32,
}
```

#### Message

```
{
    source: "a" if mainnet else "b",
    connectionId: action_hash,
}
```

#### Wire signature

```json
{ "r": "0x<64hex>", "s": "0x<64hex>", "v": 27|28 }
```

### 3.2 既知ベクタ (signing_test.py から)

参照: <https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/tests/signing_test.py>

全テスト共通の private key:

```
0x0123456789012345678901234567890123456789012345678901234567890123
```

PR-B1 で検証する **5 件の L1 action ベクタ**:

| # | テスト名 | action | nonce | vault | mainnet (r/s/v) | testnet (r/s/v) |
|---|---|---|---|---|---|---|
| 1 | `test_l1_action_signing_matches` | dummy | 0 | None | `0x53749d.../0x755c40.../27` | `0x542af6.../0x17b8b3.../28` |
| 2 | `test_l1_action_signing_order_matches` | order ETH/buy/100/100/Gtc | 0 | None | `0xd65369.../0x2b5411.../28` | `0x82b2ba.../0x6b5387.../27` |
| 3 | `test_l1_action_signing_order_with_cloid_matches` | order + cloid `0x...01` | 0 | None | `0x041ae1.../0x3c61f6.../27` | `0xeba066.../0x7f3e74.../28` |
| 4 | `test_l1_action_signing_matches_with_vault` | dummy | 0 | `0x171988...d775ea` | `0x03c548.../0x4d402b.../28` | `0xe281d2.../0x7ddad2.../27` |
| 5 | `test_schedule_cancel_action` (basic) | scheduleCancel | 0 | None | `0x6cdfb2.../0x6557ac.../27` | `0xc75bb1.../0x342f8e.../28` |

User-signed action (UsdSend / Withdraw / SubAccount transfer) は本 PR スコープ外.

### 3.3 公式 doc の補強

参照: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/signing>

- L1 action と user-signed action の 2 系統の存在を強調
- field order, trailing zero, address case の典型エラー記述

## 4. 技術スタック (Gemini deep review 確定, 2026-05-05)

| 項目 | 採用 | バージョン目安 | 根拠 |
|---|---|---|---|
| crate 構成 | `alloy` meta crate + `default-features = false` | latest stable (1.x 系) | 個別 crate の version 不一致を回避, Cargo.lock 安定 |
| EIP-712 | `sol!` macro + `alloy_signer::sign_typed_data` | alloy-sol-types 1.4+ | 自前 `SolStruct` 実装は既知ベクタ一致まで時間浪費 |
| msgpack | **struct + `#[derive(Serialize)]` で field 宣言順を Python dict 順と完全一致** | rmp-serde 1.x | dict insertion order 仕様の唯一の安全な再現法 |
| chainId | 1337 固定 (mainnet/testnet 共通) | — | HL spec 確定 |
| signature wire | y_parity → +27 して `{r,s,v}` JSON DTO | — | HL は r/s/v 分離 hex 形式を要求 |
| テストベクタ | Python 一回実行で JSON dump → commit, Rust は読み込みのみ | — | CI 決定性 + Python ランタイム不要 |
| PK 管理 | `secrecy::SecretString` で受領 → `PrivateKeySigner` 即時封入 | secrecy 0.10 (workspace) | 生 PK のメモリ滞在最小化 |

### 4.1 追加 workspace 依存

crates.io 確認 (2026-05-05) で latest stable は以下:

| crate | latest stable | 備考 |
|---|---|---|
| `alloy` (meta) | **2.0.4** | 2.0.2/2.0.3 は yanked. 2.0.4 が active. rust-version=1.91 要求 |
| `alloy-sol-types` | 1.5.7 | meta 経由で自動取得 (個別指定不要) |
| `alloy-signer-local` | 2.0.4 | meta 経由で自動取得 |
| `rmp-serde` | **1.3.1** | 安定 |
| `hex` | **0.4.3** | 安定 |

`executor/Cargo.toml` の `[workspace.dependencies]` に追加:

```toml
alloy = { version = "2.0.4", default-features = false, features = ["signer-local", "sol-types", "signers"] }
rmp-serde = "1.3.1"
hex = "0.4.3"
```

`executor-hl/Cargo.toml` の `[dependencies]` で workspace 経由参照.

### 4.2 Rust MSRV bump

alloy 2.x が rust-version=1.91 を要求するため, workspace の MSRV を 1.85 → **1.91** に bump する.

```toml
# executor/Cargo.toml [workspace.package]
rust-version = "1.91"
```

確認:
- ローカル開発環境: `rustup update stable` で 1.91+ を確保 (現状 1.95+ で問題なし)
- CI: `.github/workflows/ci.yml` は `dtolnay/rust-toolchain@stable` で最新 stable を取得 → 1.95+ 自動的に満たす
- HANDOFF doc には 1.85 と記録されているが PR-B1 で更新する旨を引き継ぐ

### 4.3 ライセンス確認

新規追加の crate は全て workspace の Apache-2.0 と互換:
- `alloy` ファミリー: MIT OR Apache-2.0
- `rmp-serde`: MIT
- `hex`: MIT OR Apache-2.0

## 5. 実装設計

### 5.1 ファイル構成

| パス | 役割 | アクション |
|---|---|---|
| `executor/crates/executor-hl/src/signer.rs` | `Signer` trait + `MockSigner` (既存) + **新規 `Eip712AgentSigner`** | 拡張 |
| `executor/crates/executor-hl/src/eip712.rs` | EIP-712 typed-data 定義 (sol! macro による Agent struct) と action_hash 計算 | **新規** |
| `executor/crates/executor-hl/tests/signing_cross_check.rs` | 既知ベクタ 5 件で `Eip712AgentSigner` の出力を assert | **新規** |
| `executor/crates/executor-hl/tests/fixtures/signing/known_vectors.json` | Python 生成済の `(action, nonce, vault, mainnet, expected_r, expected_s, expected_v)` ペア 10 件 (5 ベクタ × mainnet/testnet) | **新規** |
| `scripts/gen_signing_vectors.py` | hyperliquid-python-sdk を venv に入れて known vectors を fixture JSON に dump | **新規** |
| `executor/crates/executor-hl/Cargo.toml` | `alloy`, `rmp-serde`, `hex` 依存追加 | 修正 |
| `executor/Cargo.toml` | workspace deps に `alloy`, `rmp-serde`, `hex` 追加 | 修正 |

### 5.2 `eip712.rs` モジュール

```rust
//! HL L1 action EIP-712 typed-data + action_hash.
//!
//! HL python-sdk 0.23.0 master 互換. cross-check ベクタは
//! tests/signing_cross_check.rs を参照.

use alloy::sol_types::{eip712_domain, sol, Eip712Domain, SolStruct};
use alloy_primitives::{keccak256, Address, B256, U256};
use serde::Serialize;

sol! {
    #[derive(Debug, Serialize)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

/// HL L1 EIP-712 domain (固定. mainnet/testnet 共通で chainId=1337).
pub fn l1_domain() -> Eip712Domain {
    eip712_domain! {
        name: "Exchange",
        version: "1",
        chain_id: 1337,
        verifying_contract: Address::ZERO,
    }
}

/// action_hash = keccak256(msgpack(action) || nonce_be8 || vault_flag || expires_flag).
///
/// `action` は `serde::Serialize` で msgpack 化される.
/// **field 宣言順は HL python-sdk が dict に入れる順番と完全一致させること.**
/// HL の各 action 型 (order / cancel / scheduleCancel / dummy) ごとに struct を定義する.
pub fn action_hash<T: Serialize>(
    action: &T,
    nonce: u64,
    vault_address: Option<&Address>,
    expires_after: Option<u64>,
) -> Result<B256, rmp_serde::encode::Error> {
    let mut buf = rmp_serde::to_vec(action)?;
    buf.extend_from_slice(&nonce.to_be_bytes());
    match vault_address {
        None => buf.push(0x00),
        Some(addr) => {
            buf.push(0x01);
            buf.extend_from_slice(addr.as_slice());
        }
    }
    if let Some(exp) = expires_after {
        buf.push(0x00);
        buf.extend_from_slice(&exp.to_be_bytes());
    }
    Ok(keccak256(&buf))
}

/// Agent message: source = "a" (mainnet) | "b" (testnet), connectionId = action_hash
pub fn build_agent(action_hash: B256, is_mainnet: bool) -> Agent {
    Agent {
        source: if is_mainnet { "a" } else { "b" }.into(),
        connectionId: action_hash,
    }
}
```

### 5.3 `signer.rs` 拡張

```rust
/// 実 EIP-712 署名を行う agent wallet signer.
pub struct Eip712AgentSigner {
    inner: alloy_signer_local::PrivateKeySigner,
    is_mainnet: bool,
}

impl Eip712AgentSigner {
    /// Construct from a private key (secret string `0x` + 64 hex).
    pub fn from_secret(pk: secrecy::SecretString, is_mainnet: bool) -> Result<Self, HlError> {
        use secrecy::ExposeSecret;
        let signer: alloy_signer_local::PrivateKeySigner = pk
            .expose_secret()
            .parse()
            .map_err(|e| HlError::InvalidConfig(format!("agent PK parse: {e}")))?;
        Ok(Self { inner: signer, is_mainnet })
    }
}

#[async_trait]
impl Signer for Eip712AgentSigner {
    fn address(&self) -> Address {
        Address::new(format!("{:?}", self.inner.address()))
    }

    async fn sign_l1(&self, action: &Action, nonce: u64) -> Result<Signature, HlError> {
        // 1. action は serde_json::Value から HL action struct (Serialize) に変換
        //    → ここは action 別に struct を定義する dispatcher が必要
        //    PR-B1 では既知ベクタ用の DummyAction / OrderAction / ScheduleCancelAction を実装
        // 2. action_hash 計算 (vault, expires は本 PR では None 固定)
        // 3. Agent message 構築
        // 4. PrivateKeySigner::sign_typed_data で署名
        // 5. y_parity から v 算出 (27 + parity)
        // 6. Signature { r, s, v } に変換
        todo!("実装は Task 4 で")
    }
}
```

### 5.4 action struct 定義

PR-B1 で既知ベクタを通すために必要な struct (HL python-sdk dict 順と一致):

```rust
// dummy action: {"type": "dummy", "num": <int>}
#[derive(Debug, Serialize)]
struct DummyAction {
    #[serde(rename = "type")]
    action_type: &'static str,  // "dummy"
    num: u64,                    // float_to_int_for_hashing(1000) = 100_000_000_000
}

// order action: {"type": "order", "orders": [...], "grouping": "na"}
#[derive(Debug, Serialize)]
struct OrderAction {
    #[serde(rename = "type")]
    action_type: &'static str,  // "order"
    orders: Vec<OrderWire>,
    grouping: &'static str,      // "na"
}

// order wire: dict order = a, b, p, s, r, t, [c]
#[derive(Debug, Serialize)]
struct OrderWire {
    a: u32,                        // asset index
    b: bool,                       // is_buy
    p: String,                     // limit_px (float_to_wire)
    s: String,                     // sz (float_to_wire)
    r: bool,                       // reduce_only
    t: OrderTypeWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    c: Option<String>,             // cloid hex (only when present)
}

// order type: {"limit": {"tif": "Gtc"|"Alo"|"Ioc"}}
#[derive(Debug, Serialize)]
struct OrderTypeWire {
    limit: LimitTif,
}

#[derive(Debug, Serialize)]
struct LimitTif {
    tif: &'static str,
}

// scheduleCancel: {"type": "scheduleCancel"} or {"type": "scheduleCancel", "time": <ms>}
#[derive(Debug, Serialize)]
struct ScheduleCancelAction {
    #[serde(rename = "type")]
    action_type: &'static str,  // "scheduleCancel"
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<u64>,
}
```

> **field 順は HL python-sdk の dict 構築順と一致させること.** 例えば `OrderWire` は
> `a, b, p, s, r, t, c` の順. これを変えると msgpack 出力 byte 列が変わって既知ベクタが
> 通らない. 順序変更は将来 SDK が変えた時のみ追従する.

### 5.5 既知ベクタ生成 (`scripts/gen_signing_vectors.py`)

```python
#!/usr/bin/env python3
"""Generate known signing vectors from hyperliquid-python-sdk for Rust cross-check.

Usage:
    python3 -m venv .venv-hl-sdk
    source .venv-hl-sdk/bin/activate
    pip install hyperliquid-python-sdk msgpack eth-account
    python3 scripts/gen_signing_vectors.py > executor/crates/executor-hl/tests/fixtures/signing/known_vectors.json
"""
import json
import eth_account
from hyperliquid.utils.signing import (
    sign_l1_action, order_request_to_order_wire,
    order_wires_to_order_action, float_to_int_for_hashing,
)
from hyperliquid.utils.types import Cloid

PK = "0x0123456789012345678901234567890123456789012345678901234567890123"
wallet = eth_account.Account.from_key(PK)

VECTORS = []

def emit(name, action, nonce, vault, expires, is_mainnet):
    sig = sign_l1_action(wallet, action, vault, nonce, expires, is_mainnet)
    VECTORS.append({
        "name": name,
        "action": action,
        "nonce": nonce,
        "vault_address": vault,
        "expires_after": expires,
        "is_mainnet": is_mainnet,
        "expected_r": sig["r"],
        "expected_s": sig["s"],
        "expected_v": sig["v"],
        "expected_address": wallet.address.lower(),
    })

# Vector 1: dummy action
emit("dummy", {"type": "dummy", "num": float_to_int_for_hashing(1000)}, 0, None, None, True)
emit("dummy_testnet", {"type": "dummy", "num": float_to_int_for_hashing(1000)}, 0, None, None, False)

# Vector 2: order
order = order_request_to_order_wire({
    "coin": "ETH", "is_buy": True, "sz": 100, "limit_px": 100,
    "reduce_only": False,
    "order_type": {"limit": {"tif": "Gtc"}},
    "cloid": None,
}, 1)
order_action = order_wires_to_order_action([order])
emit("order_eth", order_action, 0, None, None, True)
emit("order_eth_testnet", order_action, 0, None, None, False)

# Vector 3: order with cloid
order_c = order_request_to_order_wire({
    "coin": "ETH", "is_buy": True, "sz": 100, "limit_px": 100,
    "reduce_only": False,
    "order_type": {"limit": {"tif": "Gtc"}},
    "cloid": Cloid.from_str("0x00000000000000000000000000000001"),
}, 1)
emit("order_with_cloid", order_wires_to_order_action([order_c]), 0, None, None, True)
emit("order_with_cloid_testnet", order_wires_to_order_action([order_c]), 0, None, None, False)

# Vector 4: dummy with vault
emit("dummy_with_vault", {"type": "dummy", "num": float_to_int_for_hashing(1000)}, 0,
     "0x1719884eb866cb12b2287399b15f7db5e7d775ea", None, True)
emit("dummy_with_vault_testnet", {"type": "dummy", "num": float_to_int_for_hashing(1000)}, 0,
     "0x1719884eb866cb12b2287399b15f7db5e7d775ea", None, False)

# Vector 5: scheduleCancel
emit("schedule_cancel", {"type": "scheduleCancel"}, 0, None, None, True)
emit("schedule_cancel_testnet", {"type": "scheduleCancel"}, 0, None, None, False)

print(json.dumps(VECTORS, indent=2, ensure_ascii=False))
```

実行手順は `docs/HANDOFF-2026-05-04.md` の同階層に追記する.

### 5.6 cross-check テスト

```rust
// executor/crates/executor-hl/tests/signing_cross_check.rs
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::signer::{Eip712AgentSigner, Signer};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    action: serde_json::Value,
    nonce: u64,
    vault_address: Option<String>,
    expires_after: Option<u64>,
    is_mainnet: bool,
    expected_r: String,
    expected_s: String,
    expected_v: u8,
    expected_address: String,
}

fn vectors() -> Vec<Vector> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/signing/known_vectors.json");
    let s = std::fs::read_to_string(&p).unwrap();
    serde_json::from_str(&s).unwrap()
}

const TEST_PK: &str =
    "0x0123456789012345678901234567890123456789012345678901234567890123";

#[tokio::test]
async fn cross_check_all_known_vectors() {
    use secrecy::SecretString;
    for v in vectors() {
        let signer = Eip712AgentSigner::from_secret(
            SecretString::new(TEST_PK.into()),
            v.is_mainnet,
        ).unwrap();
        // assert addr
        assert_eq!(signer.address().as_str().to_lowercase(), v.expected_address);
        // dispatch action via internal mapper (Task 4 で実装)
        let sig = signer.sign_l1(&v.action, v.nonce).await.unwrap();
        assert_eq!(sig.r, v.expected_r, "{} r mismatch", v.name);
        assert_eq!(sig.s, v.expected_s, "{} s mismatch", v.name);
        assert_eq!(sig.v, v.expected_v, "{} v mismatch", v.name);
    }
}
```

### 5.7 受け入れ基準

- [ ] `python3 scripts/gen_signing_vectors.py > tests/fixtures/signing/known_vectors.json` で 10 件 (5 vector × mainnet/testnet) の JSON 生成
- [ ] `cargo test -p executor-hl --test signing_cross_check` で 10/10 一致
- [ ] `cargo test --workspace` で既存 126 テスト + 1 cross-check = 127 全 pass
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] CI green

## 6. 既知の落とし穴 (Gemini 指摘 + 設計時注意)

### 6.1 msgpack field order — 最大バグ温床

Python `dict` は **insertion order**, Rust `serde` の struct は **field 宣言順**.
両者を一言一句一致させること. action struct のフィールド並びを変えるバージョン bump 時は
既知ベクタが必ず壊れるので, 同じ commit で fixture 再生成 + Rust struct 修正を行う.

### 6.2 v の y_parity vs 27/28

alloy の `Signature::v()` は実装によって `0/1` (y_parity) を返す場合と `27/28` を返す場合あり.
HL は **必ず 27 か 28** を要求. 実装時に `if sig.v().y_parity() { 28 } else { 27 }` 形で
明示的に変換する. `y_parity` の取り方は alloy の version で API が違うので, テストで実値を
確認してから固定する.

### 6.3 chainId 1337 の不変

mainnet も testnet も chainId=1337. Arbitrum chainId (42161) や testnet chainId (421614) と
混同しない. これは HL 内部 L2 の固定値.

### 6.4 source 値 ("a"/"b") は **L1 action のみ**

User-signed action では `hyperliquidChain: "Mainnet"|"Testnet"` を action 本体に追加する仕様.
本 PR はそこに踏み込まないが, 将来の `Eip712UserSigner` では別実装になることを意識.

### 6.5 verifyingContract = ZeroAddress

`0x0000000000000000000000000000000000000000` 固定. 一見不自然だが, HL がこの慣習で運用しているため
そのまま使う. EIP-712 仕様としては有効.

### 6.6 alloy bump 時の API drift

alloy 1.x → 2.x の breaking change で `sign_typed_data` の戻り型や `Signature` の getter が変わる
可能性あり. workspace deps の version は patch まで pin. 上げるときは known vectors を再 run.

## 7. リスク評価

| リスク | 影響 | 対策 |
|---|---|---|
| msgpack 一致しない | 既知ベクタ全 fail | struct field 順 + 型を Python dict 完全一致, 失敗時の diff 確認手順を doc 化 |
| EIP-712 hash 不一致 | 5 vector で fail | sol! macro 使用で実装ミス排除, それでも fail なら hashStruct を手動 dump して Python と byte 比較 |
| alloy version mismatch | コンパイル不能 / 挙動変化 | workspace deps で patch まで pin, lock file で再現 |
| PK 漏洩 | 重大 | secrecy::SecretString + Claude PreToolUse hook で sign_secret 系コマンド block 継続 |

## 8. 関連リンク

- 親 spec: [`2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md`](2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md)
- HL python-sdk signing.py: <https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/utils/signing.py>
- HL python-sdk signing_test.py: <https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/tests/signing_test.py>
- HL exchange endpoint doc: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>
- HL signing doc: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/signing>
- alloy: <https://github.com/alloy-rs/alloy>
- alloy-sol-types Eip712Domain: <https://docs.rs/alloy-sol-types/latest/alloy_sol_types/macro.eip712_domain.html>
