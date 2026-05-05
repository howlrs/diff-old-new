# PR-C1: MetaCache + executor-server real mode 切替 設計

**作成日**: 2026-05-05
**ブランチ**: `feat/pr-c1-meta-cache-real-mode` (実装時に作成)
**親 spec**: `docs/superpowers/specs/2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md` Stage C (production path 移行)
**前提コード**: PR-B2b merged (`develop@286794c`)

## 1. 目的

`OrderIntent` / `CancelIntent` から HL 固有の wire detail である `asset: u32` field を除去し,
executor-hl 内部の `MetaCache` で symbol → asset index 解決を一元化する.
合わせて `executor-server` を mock mode から real mode (mainnet/testnet) に起動時切替できるように
clap CLI flag を追加し, 起動時に `fetch_meta()` で MetaCache を build する.

これにより:
- algo runtime caller (16 件) の `// TODO(PR-B2b): resolve via meta cache` placeholder が消える
- domain type が wire format から完全分離される (Gemini Q3 の指摘した Leaky Abstraction の解消)
- production path での **「asset=0 で BTC 誤発注」リスクが構造的に排除** される

## 2. 非目的

- symbol allowlist (PR-C2)
- size cap server-内蔵 (PR-C2)
- baseline-diff guard 自動 emergency_stop (PR-C3)
- emergency_stop multi-symbol live test (PR-C4)
- mainnet 投入の本格運用 (PR-C2/C3 完了後)

## 3. 制約と前提

### 3.1 既存コード状況 (2026-05-05, PR-B2b merged 後)

| 項目 | 現状 |
|---|---|
| `executor-core::intent::OrderIntent` | `pub asset: u32` 含む 8 fields (PR-B2a で追加) |
| `executor-core::intent::CancelIntent` | `pub asset: u32` 含む (同) |
| algo runtime callers | 16 件で `OrderIntent { asset: 0, ... }` + `// TODO(PR-B2b)` |
| test fixture callers | 5 件で `asset: 0` (mock backend, 検証なし) |
| executor-server `main.rs` | `MockHlClient` + `MockSigner` 固定 |
| `executor-hl::wire::WireMeta` | PR-A 実装済. universe: Vec<{name, sz_decimals, max_leverage, only_isolated}> |
| `RealHlClient::fetch_meta(dex: Option<&str>)` | PR-A 実装済 (live mainnet で実証済) |
| `Eip712AgentSigner` | PR-B1 実装済, byte-identical to HL python-sdk |
| `live_mainnet_place_cancel.rs` | PR-B2b で `intent.asset = eth_idx` (動的取得) |

### 3.2 Gemini deep review の決定的判断 (2026-05-05)

8 項目に対する gemini-3.1-pro-preview の推奨を全採用:

| 項目 | 採用 |
|---|---|
| Q1 MetaCache 置き場 | `RealHlClient` struct のフィールド `Arc<MetaCache>` |
| Q2 Symbol 型 | 現状 `pub struct Symbol(pub String)` 維持 (HashMap key として動作) |
| Q3 Symbol enum 化 | しない (YAGNI; 50+ caller 影響回避) |
| Q4 CLI flag | `--mode mock\|real`, `--base mainnet\|testnet` のみ追加 |
| Q5 Cancel asset 解決 | `RealHlClient::resolve_asset(&self, symbol)` private helper で place/cancel 共通化 |
| Q6 MockHlClient 対応 | MetaCache 持たず内部固定値 `1` fallback |
| Q7 HlError variant | `HlError::UnknownSymbol(Symbol)` 新設 (string match 回避) |
| Q8 PR スコープ | 1 PR で完結 (compiler-driven 強凝集変更, 分割すると半端状態) |

## 4. 設計

### 4.1 `MetaCache` (executor-hl/src/meta.rs 新設)

```rust
//! HL universe symbol → asset index cache.
//!
//! Built once at startup from /info meta endpoint(s). HL universe additions
//! (new coin listings) require process restart — explicit, fail-safe operation.

use crate::errors::HlError;
use crate::hl_client::HlClient;
use crate::wire::WireMeta;
use executor_core::symbol::Symbol;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct MetaCache {
    /// "ETH" → 1, "BTC" → 0, "xyz:META" → ... etc.
    /// HIP-3 dex prefixes (`xyz:META`) are stored as-is; the prefix is part of the key.
    by_symbol: HashMap<Symbol, u32>,
}

impl MetaCache {
    /// Build cache from one or more dex universes. Pass `dexes = &[None]` for default
    /// dex only; `&[None, Some("xyz")]` to include the xyz HIP-3 dex; etc.
    pub async fn build(client: &dyn HlClient, dexes: &[Option<&str>]) -> Result<Self, HlError> {
        let mut by_symbol = HashMap::new();
        for dex in dexes {
            let meta: WireMeta = client.fetch_meta(*dex).await?;
            for (idx, entry) in meta.universe.iter().enumerate() {
                let key = match dex {
                    None => Symbol::new(&entry.name),
                    Some(d) => Symbol::new(format!("{d}:{}", entry.name)),
                };
                by_symbol.insert(key, idx as u32);
            }
        }
        Ok(Self { by_symbol })
    }

    /// Resolve a symbol; returns `Err(UnknownSymbol)` if not found.
    pub fn resolve(&self, symbol: &Symbol) -> Result<u32, HlError> {
        self.by_symbol
            .get(symbol)
            .copied()
            .ok_or_else(|| HlError::UnknownSymbol(symbol.clone()))
    }

    /// Number of symbols cached (for diagnostic / startup log).
    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}
```

### 4.2 `OrderIntent` / `CancelIntent` の field 削除

```rust
// executor-core/src/intent.rs
pub struct OrderIntent {
    pub cloid: Cloid,
    pub symbol: Symbol,
    // pub asset: u32,   // ← 削除
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
}

pub struct CancelIntent {
    pub symbol: Symbol,
    // pub asset: u32,   // ← 削除
    pub by_cloid: Option<Cloid>,
    pub by_oid: Option<OrderId>,
}
```

PR-B2a で追加した `asset` field を撤回. 21 caller を 1 commit で全修正.

### 4.3 `HlError::UnknownSymbol` variant 追加

```rust
#[derive(Debug, thiserror::Error)]
pub enum HlError {
    // ... (既存)

    #[error("unknown symbol (not in MetaCache): {0}")]
    UnknownSymbol(executor_core::symbol::Symbol),

    // ... (既存)
}
```

`Display` impl は `Symbol::Display` (PR-A 実装済) に依存.

### 4.4 `RealHlClient` の MetaCache 統合

```rust
pub struct RealHlClient {
    pub config: HlConfig,
    pub signer: Arc<dyn Signer>,
    pub rate_limiter: Arc<TokenBucket>,
    pub http: reqwest::Client,
    /// PR-C1: pre-built symbol → asset cache. Populated at startup.
    /// `None` only during transitional construction (e.g. `fetch_meta` itself
    /// being called to BUILD the cache); after `with_meta` it must be Some.
    pub meta: Arc<MetaCache>,
}

impl RealHlClient {
    /// Construct WITHOUT MetaCache (for the initial fetch_meta call only).
    /// Most production code should use `with_meta` after building the cache.
    pub fn bootstrap(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
        let http = /* existing */ ;
        Self {
            config,
            signer,
            rate_limiter: Arc::new(TokenBucket::hyperliquid_default()),
            http,
            meta: Arc::new(MetaCache { by_symbol: HashMap::new() }), // empty stub
        }
    }

    /// Replace the MetaCache after build. Returns a new `RealHlClient` so the
    /// old bootstrap one can be dropped (or call `Arc::make_mut` to mutate).
    pub fn with_meta(self, meta: Arc<MetaCache>) -> Self {
        Self { meta, ..self }
    }

    /// Private helper used by both place_orders and cancel_orders.
    fn resolve_asset(&self, symbol: &Symbol) -> Result<u32, HlError> {
        self.meta.resolve(symbol)
    }
}
```

`place_orders` / `cancel_orders` の wire 構築箇所:

```rust
async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
    if orders.is_empty() { return Ok(vec![]); }
    // ... rate limit ...

    let mut order_wires: Vec<OrderWire> = Vec::with_capacity(orders.len());
    let mut dropped: Vec<OrderResponse> = Vec::new();
    for intent in orders {
        match self.resolve_asset(&intent.symbol) {
            Ok(asset) => order_wires.push(intent_to_wire(intent, asset)),
            Err(HlError::UnknownSymbol(_)) => {
                tracing::error!(symbol=%intent.symbol, "unknown symbol; dropping order");
                dropped.push(OrderResponse {
                    cloid: intent.cloid,
                    oid: None,
                    status: "error".into(),
                    error: Some(format!("unknown symbol: {}", intent.symbol)),
                });
            }
            Err(e) => return Err(e),
        }
    }

    if order_wires.is_empty() {
        return Ok(dropped); // all dropped
    }

    // ... build action / sign / post / parse ...
    let mut placed = parse_exchange_response(&resp_text, ...)?;
    placed.extend(dropped); // merge dropped responses preserving caller order
    // (or use a Vec<Option<usize>> mapping for true order preservation)
    Ok(placed)
}
```

注意: dropped order の挿入位置を caller-order と一致させるには `Vec<Option<usize>>` で
mapping を保持する. 単純な `extend` は順序が壊れる. 詳細は plan で.

### 4.5 `MockHlClient` の対応

```rust
impl MockHlClient {
    // 既存
}

impl HlClient for MockHlClient {
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        // PR-C1: asset field is gone. Mock just stores the intent and returns
        // a synthetic response. asset resolution is not tested here.
        // ...
    }
}
```

`OrderWire` 構築は不要 (mock は wire を扱わない). `seed_meta` のような機能は YAGNI.

### 4.6 BatchSender への影響

`BatchSender` 自体は `OrderIntent` を持ち回すだけで wire format を扱わない (PR-A 設計).
PR-C1 では BatchSender の signature に変更なし. ただし internal で `OrderIntent.asset` を
read していた箇所があれば削除 (現状は無いはず, plan で確認).

### 4.7 executor-server `main.rs` の clap 化

```rust
use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "executor-server", version)]
struct Args {
    /// Backend mode: mock for CI/test, real for mainnet/testnet.
    #[arg(long, default_value = "mock")]
    mode: Mode,

    /// HL endpoint base. Only relevant when mode=real.
    #[arg(long, default_value = "mainnet")]
    base: Base,

    /// Bind address.
    #[arg(long, env = "EXECUTOR_BIND", default_value = "0.0.0.0:8085")]
    bind: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Mock,
    Real,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Base {
    Mainnet,
    Testnet,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // ... tracing init ...

    let app_state = Arc::new(AppState::new());
    let (hl_client, signer): (Arc<dyn HlClient>, Arc<dyn Signer>) = match args.mode {
        Mode::Mock => {
            let mock_hl = Arc::new(MockHlClient::new());
            let signer = Arc::new(MockSigner::new());
            (mock_hl, signer)
        }
        Mode::Real => {
            let config = match args.base {
                Base::Mainnet => HlConfig::mainnet(),
                Base::Testnet => HlConfig::testnet(),
            };
            let pk = std::env::var("HL_AGENT_PK")
                .context("HL_AGENT_PK env required for --mode real")?;
            let is_mainnet = matches!(args.base, Base::Mainnet);
            let signer: Arc<dyn Signer> = Arc::new(
                Eip712AgentSigner::from_secret(SecretString::new(pk.into()), is_mainnet)?
            );
            // Bootstrap client without meta, build cache, then upgrade.
            let bootstrap_client = RealHlClient::bootstrap(config.clone(), signer.clone());
            let dexes: &[Option<&str>] = &[None]; // PR-C1 scope: default dex only.
            let meta = Arc::new(MetaCache::build(&bootstrap_client, dexes).await?);
            tracing::info!(symbols = meta.len(), "MetaCache built");
            let real_client = Arc::new(bootstrap_client.with_meta(meta));
            (real_client, signer)
        }
    };

    // ... batch_sender + ServerState + axum bind (既存と同じ) ...
}
```

### 4.8 caller 修正 (21 件)

algo runtime 16 + test fixture 5 = 計 21 caller の `OrderIntent { asset: 0, ... }` から
`asset: 0,` 行を削除. `// TODO(PR-B2b)` コメントも除去.

PR-A の `live_mainnet_place_cancel.rs` (PR-B2b) の `OrderIntent { asset: eth_idx, ... }` も
同様に `asset` 行と eth_idx 計算ロジックを削除. 既存の `fetch_meta` 呼び出しは残す
(diagnostic eprintln 用に).

`live_mainnet_place_cancel.rs` 修正後:

```rust
// 既存 fetch_meta の呼び出しは残す (eprintln 診断用)
let meta = client.fetch_meta(None).await.expect("fetch meta");
let _eth_idx = meta.universe.iter().position(|u| u.name == "ETH"); // optional eprintln only

// OrderIntent から asset 行を削除
let intent = OrderIntent {
    cloid,
    symbol: Symbol::new("ETH"),
    side: Side::Long,
    px: order_px,
    sz: order_sz,
    tif: Tif::Alo,
    reduce_only: false,
};
```

### 4.9 ファイル構成

| パス | 役割 | アクション |
|---|---|---|
| `executor/crates/executor-hl/src/meta.rs` | NEW. `MetaCache` struct + build/resolve | Create |
| `executor/crates/executor-hl/src/lib.rs` | `pub mod meta;` 追加 | Modify |
| `executor/crates/executor-hl/src/errors.rs` | `UnknownSymbol(Symbol)` variant 追加 | Modify |
| `executor/crates/executor-hl/src/hl_client.rs` | RealHlClient に `meta: Arc<MetaCache>` field, `bootstrap()` / `with_meta()` / `resolve_asset()`. `place_orders` / `cancel_orders` の wire 構築で resolve | Modify |
| `executor/crates/executor-hl/src/eip712.rs` | `order_intent_to_wire` / `cancel_intent_to_wire` のシグネチャに asset 引数を追加 (intent からは取れなくなるため) | Modify |
| `executor/crates/executor-core/src/intent.rs` | `OrderIntent.asset` / `CancelIntent.asset` field 削除 | Modify |
| `executor/crates/executor-algo/src/{market,market_make,passive_follow,twap}.rs` | 16 caller で `asset: 0,` + `// TODO(PR-B2b)` 削除 | Modify |
| `executor/crates/executor-hl/src/{batch_sender,hl_client,ws_state}.rs` (test fixtures) | 5 caller で `asset: 0,` 削除 | Modify |
| `executor/crates/executor-hl/tests/place_cancel_mock.rs` | test fixture の `asset: 1,` 削除 | Modify |
| `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs` | `intent.asset = eth_idx` 削除 (eth_idx 計算は eprintln 用に残す or 全削除) | Modify |
| `executor/crates/executor-server/src/main.rs` | clap 導入 (`--mode` `--base`), real mode 起動 path | Modify |
| `executor/Cargo.toml` | `clap` を `[workspace.dependencies]` に追加 (executor-cli は既に使用, version 統一) | Modify |
| `executor/crates/executor-server/Cargo.toml` | `clap = { workspace = true, features = ["derive"] }` 追加 | Modify |

### 4.10 mock backend test の resolve 検証

`MockHlClient` は MetaCache 持たない. `place_cancel_mock.rs` テスト 8 件は `asset: 1` を削除しても
mock は内部で wire を作らず `OrderResponse` を返すだけなので動作変化なし.

新規テスト 1 件追加 (UnknownSymbol path):

```rust
// place_cancel_mock.rs に追加
#[tokio::test]
async fn place_orders_unknown_symbol_returns_error_response() {
    // RealHlClient with EMPTY MetaCache. Any symbol → UnknownSymbol.
    let signer = Arc::new(Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap());
    let mut server = mockito::Server::new_async().await;
    // No mock needed — drop happens before HTTP.
    let config = HlConfig {
        info_url: format!("{}/info", server.url()),
        exchange_url: format!("{}/exchange", server.url()),
        ws_url: "ws://unused".into(),
    };
    let client = RealHlClient::bootstrap(config, signer); // empty meta
    let intent = make_order_intent(); // symbol "ETH"
    let resp = client.place_orders(&[intent]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "error");
    assert!(resp[0].error.as_deref().unwrap().contains("unknown symbol"));
}
```

`bootstrap` が空 MetaCache を持つ前提. すべての order が UnknownSymbol → drop + error response.
HTTP は呼ばれない (resolve が先に fail).

### 4.11 受け入れ基準

- [ ] `cargo test --workspace` で 142 + 1 (UnknownSymbol test) = 143 tests pass
- [ ] `cargo test -p executor-hl --features live live_mainnet_place_cancel` を user が再走行 → mainnet で再度 round trip 成功 (asset 削除後)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `executor-server --mode mock` で従来通り起動 (CI 既存テスト pass)
- [ ] `executor-server --mode real --base mainnet` (ユーザー手動検証, MetaCache len > 0 ログ確認)
- [ ] `OrderIntent` / `CancelIntent` から `asset` field が完全に削除されている (grep で 0 件)
- [ ] `// TODO(PR-B2b)` が algo crate 内に 1 件も残らない (grep で 0 件)
- [ ] Gemini deep review pass (MUST-FIX なし or 全対応)

## 5. 実装の重要な注意点

### 5.1 dropped order の caller-order preservation

`place_orders` で UnknownSymbol を skip → `OrderResponse` として error を返すが, 順序を
`orders` の input 順序と一致させる必要あり. 単純な `Vec.extend` は壊れる.

実装方針:
```rust
let mut responses: Vec<Option<OrderResponse>> = vec![None; orders.len()];
let mut wires_with_idx: Vec<(usize, OrderWire)> = Vec::new();
for (i, intent) in orders.iter().enumerate() {
    match self.resolve_asset(&intent.symbol) {
        Ok(asset) => wires_with_idx.push((i, intent_to_wire(intent, asset))),
        Err(HlError::UnknownSymbol(_)) => {
            responses[i] = Some(OrderResponse {
                cloid: intent.cloid,
                oid: None,
                status: "error".into(),
                error: Some(format!("unknown symbol: {}", intent.symbol)),
            });
        }
        Err(e) => return Err(e),
    }
}
// HTTP only if any wires survived.
if !wires_with_idx.is_empty() {
    let order_wires: Vec<OrderWire> = wires_with_idx.iter().map(|(_, w)| w.clone()).collect();
    // ... sign + post + parse_exchange_response ...
    // parse returns Vec<OrderResponse> in order of order_wires.
    let parsed = parse_exchange_response(&resp_text, &orders_subset)?;
    for ((i, _), parsed_resp) in wires_with_idx.iter().zip(parsed) {
        responses[*i] = Some(parsed_resp);
    }
}
let final_responses: Vec<OrderResponse> = responses.into_iter().flatten().collect();
debug_assert_eq!(final_responses.len(), orders.len());
Ok(final_responses)
```

`parse_exchange_response` の signature: 現状は `(text, &[OrderIntent]) -> Result<Vec<OrderResponse>>`.
`OrderIntent` を直接渡すのではなく `&[Cloid]` だけを渡すように signature 変更が望ましい
(parse は cloid を `OrderResponse` に詰めるためだけに OrderIntent 全体を見ている).

cancel も同様の構造変更が要る.

### 5.2 BatchSender の wire 構築

現状 PR-B2a で `eip712::order_intent_to_wire(&OrderIntent) -> OrderWire` は intent.asset を読む.
PR-C1 で intent から asset が消えるので, `order_intent_to_wire(&OrderIntent, asset: u32) -> OrderWire`
にシグネチャ変更. caller (= RealHlClient::place_orders) で `asset` を resolve して渡す.

`cancel_intent_to_wire` (現状無いが暗黙にクロージャ内で wire 構築) も同様に extract:
```rust
fn cancel_intent_to_wire(intent: &CancelIntent, asset: u32) -> CancelByCloidWire {
    CancelByCloidWire {
        asset,
        cloid: format!("{}", intent.by_cloid.expect("by_cloid required")),
    }
}
```

### 5.3 多 dex 起動時 fetch の YAGNI

PR-C1 では `dexes: &[None]` (default dex のみ) で起動. xyz / flx 等 HIP-3 dex は将来 PR で追加.
これは algo 4 種が現状 default dex symbol (BTC, ETH 等) しか発注しないため.

### 5.4 mainnet で PR-C1 を実走行する際の影響

ユーザーが `cargo run --bin executor-server -- --mode real --base mainnet` を起動すると
HL `/info meta` を 1 リクエスト (weight 20) 消費する. rate limit 1200/min への影響は無視可能.

メイン EOA に対する HL request は MetaCache build のみで, 注文 / 残高 / ポジ取得は **しない**.
(Stage A read-only path は別途. PR-C1 は wire 経路の準備のみ).

## 6. リスク評価

| リスク | 確率 | 影響 | 緩和 |
|---|---|---|---|
| MetaCache build 失敗 (HL meta endpoint 5xx) | 低 | server 起動不能 | startup error → main exit. retry なし (fail-fast) |
| caller 修正漏れ | 極低 | コンパイル fail | compiler-driven, 1 commit で全部 |
| BatchSender 経路の OrderIntent.asset read 残存 | 中 | コンパイル fail | grep + cargo build で検出 |
| live_mainnet_place_cancel.rs の test 仕様変更 | 中 | live test fail | user 再走行で確認, plan に明記 |
| Symbol(String) の HashMap 衝突 | ゼロ | — | string-based, hash 衝突は実用上無し |
| MockHlClient の挙動変化 | 低 | 既存 test fail | mock は wire 構築しないので影響なし (要確認) |

## 7. PR-C1 後のステップ

1. **PR-C2**: symbol allowlist + size cap (CLI flag `--mainnet-allow-symbols`, `--mainnet-max-notional-usd`, server middleware で intent 受領時 gate)
2. **PR-C3**: baseline-diff guard (起動時 master EOA snapshot 保存, 周期的 check, 違反時自動 emergency_stop)
3. **PR-C4**: emergency_stop multi-symbol live test, e2e live test (executor-server 起動 → HTTP request → place → cancel)

## 8. 関連リンク

- 親 spec: [`2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md`](2026-05-05-hl-mainnet-readonly-and-minimal-order-test-design.md)
- PR-B2a spec: [`2026-05-05-pr-b2a-place-cancel-with-mock-design.md`](2026-05-05-pr-b2a-place-cancel-with-mock-design.md)
- PR-B2b spec: [`2026-05-05-pr-b2b-mainnet-place-cancel-design.md`](2026-05-05-pr-b2b-mainnet-place-cancel-design.md)
- HL meta endpoint: <https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/perpetuals>
- Gemini deep review (2026-05-05): 8 質問への決定的判断, B → D pivot 含む
