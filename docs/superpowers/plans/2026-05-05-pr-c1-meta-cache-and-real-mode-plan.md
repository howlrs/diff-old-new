# PR-C1: MetaCache + executor-server real mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `asset: u32` from `OrderIntent`/`CancelIntent`, introduce `executor-hl::meta::MetaCache` that resolves `Symbol` → asset index inside `RealHlClient` (built once at startup from `/info meta`), add clap-based `--mode mock|real` and `--base mainnet|testnet` flags to `executor-server`, and clean up all 33 `OrderIntent { ... asset: 0/1, ... }` caller sites in the workspace.

**Architecture:** A new `MetaCache` lives behind `Arc<...>` as a field on `RealHlClient`. The client builds it via a 2-step bootstrap-then-upgrade pattern (`RealHlClient::bootstrap()` → `MetaCache::build()` → `RealHlClient::with_meta()`) to break the chicken-and-egg between needing an `HlClient` to call `fetch_meta()` and needing `MetaCache` inside the client. `place_orders` and `cancel_orders` resolve each intent's symbol via a private helper `resolve_asset()`; unknown symbols become per-order error responses (no panic, no batch abort), preserving the input order via a `Vec<Option<OrderResponse>>` placeholder pattern. `MockHlClient` retains no MetaCache — its `place_orders` returns synthetic responses without ever building a wire. The `executor-server` binary gains a `clap`-derived `Args` struct with `--mode mock|real` (default `mock`) and `--base mainnet|testnet` (default `mainnet`); real mode reads `HL_AGENT_PK` from env and runs the bootstrap-then-upgrade dance at startup.

**Tech Stack:** Rust 2021 (workspace MSRV 1.91), existing `alloy 2.0.4` + `rmp-serde 1.3.1` + `hex 0.4.3` + `secrecy 0.10`. New workspace dep: `clap 4` with `derive` feature (already in workspace via `executor-cli`; PR-C1 just exposes it to executor-server). No other new deps.

---

## File Structure

| Path | Responsibility | Action |
|---|---|---|
| `executor/crates/executor-hl/src/meta.rs` | NEW. `MetaCache` struct, `build` constructor, `resolve` getter. | Create |
| `executor/crates/executor-hl/src/lib.rs` | Add `pub mod meta;` so `executor_hl::meta::MetaCache` resolves. | Modify |
| `executor/crates/executor-hl/src/errors.rs` | Add `HlError::UnknownSymbol(Symbol)` variant. | Modify |
| `executor/crates/executor-hl/src/hl_client.rs` | RealHlClient gains `meta: Arc<MetaCache>` field, `bootstrap()` constructor (empty meta), `with_meta()` upgrade, `resolve_asset()` private helper. `place_orders`/`cancel_orders` use `resolve_asset` and produce `Vec<Option<OrderResponse>>` to preserve input order across drops. | Modify |
| `executor/crates/executor-hl/src/eip712.rs` | `order_intent_to_wire`: signature changes from `(intent) -> OrderWire` to `(intent, asset: u32) -> OrderWire` because the intent no longer carries asset. Same for `CancelByCloidWire` construction (currently inline; extract to `cancel_intent_to_wire(intent, asset) -> CancelByCloidWire`). | Modify |
| `executor/crates/executor-core/src/intent.rs` | Remove `pub asset: u32` from both `OrderIntent` and `CancelIntent`. | Modify |
| `executor/crates/executor-algo/src/market.rs` | Remove `asset: 0,` line from one caller (line ~281). | Modify |
| `executor/crates/executor-algo/src/market_make.rs` | Remove `asset: 0,` from 3 callers (lines ~289, ~392, ~424; verify by build error). | Modify |
| `executor/crates/executor-algo/src/passive_follow.rs` | Remove `asset: 0,` from 5 callers (lines ~179, ~199, ~241, ~270, ~281). | Modify |
| `executor/crates/executor-algo/src/twap.rs` | Remove `asset: 0,` from 6 callers (lines ~204, ~224, ~265, ~280, ~311, ~379). | Modify |
| `executor/crates/executor-hl/src/batch_sender.rs` | Remove `asset: 0,` from 2 test fixture callers (lines ~244, ~290). | Modify |
| `executor/crates/executor-hl/src/hl_client.rs` (test fixtures) | Remove `asset: 0,` from 2 test fixture callers (lines ~779, ~792). | Modify |
| `executor/crates/executor-hl/src/ws_state.rs` | Remove `asset: 0,` from 1 test fixture caller (line ~225). | Modify |
| `executor/crates/executor-hl/src/eip712.rs` (test) | Remove `asset: 1,` from 1 test caller (line ~269). | Modify |
| `executor/crates/executor-hl/tests/place_cancel_mock.rs` | Remove `asset: 1,` from 4 callers (lines ~37, ~49, ~187, ~223). | Modify |
| `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs` | Remove `asset: eth_idx,` line + the `eth_idx` resolution code. Keep `fetch_meta()` call as a diagnostic eprintln. | Modify |
| `executor/crates/executor-server/src/routes.rs` | Remove `asset: 0,` from `emergency_stop` cancel construction (line ~266). | Modify |
| `executor/crates/executor-server/src/main.rs` | Replace bare `MockHlClient::new()` with clap `Args` parsing, mock/real branching, real mode does the bootstrap-then-upgrade dance and reads `HL_AGENT_PK` env. | Modify |
| `executor/crates/executor-server/Cargo.toml` | Add `clap = { workspace = true, features = ["derive"] }` and `secrecy = { workspace = true }` to dependencies. | Modify |
| `executor/Cargo.toml` | Promote `clap = { version = "4", features = ["derive", "env"] }` to `[workspace.dependencies]` (executor-cli currently inlines it). | Modify |
| `executor/crates/executor-cli/Cargo.toml` | Switch `clap` from inline `version = "4"` to `{ workspace = true }` so versions stay aligned. | Modify |
| `executor/crates/executor-hl/tests/place_cancel_mock.rs` (new test) | Add `place_orders_unknown_symbol_returns_error_response` test exercising `RealHlClient::bootstrap()`'s empty MetaCache. | Modify |
| `docs/HANDOFF-2026-05-04.md` | Append PR-C1 完了 subsection. | Modify |

**Why this structure:** `MetaCache` lives in `executor-hl` because resolving symbol→asset is purely a wire-format concern (decision per Gemini deep, Q1). The `bootstrap()`/`with_meta()` split keeps `RealHlClient::new()` from becoming `async` and avoids forcing every callsite to await. The compiler-driven cleanup of 33 caller sites is the central refactor; spreading it across multiple PRs would leave the build broken between commits.

---

## Task 1: Branch + workspace clap promotion

**Files:**
- Modify: `executor/Cargo.toml`
- Modify: `executor/crates/executor-cli/Cargo.toml`

- [ ] **Step 1.1: Branch from develop**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git fetch origin
git checkout develop
git pull --rebase origin develop
git checkout -b feat/pr-c1-meta-cache-real-mode
```

- [ ] **Step 1.2: Promote clap to `[workspace.dependencies]`**

Edit `executor/Cargo.toml`. Find the existing `[workspace.dependencies]` block. After the `# Testing` group's `mockito = "1.7.2"` line, add a new `# CLI parsing` group:

```toml
# CLI parsing (PR-C1: --mode/--base for executor-server)
clap = { version = "4", features = ["derive", "env"] }
```

(executor-cli currently has its own `clap = { version = "4", features = ["derive", "env"] }` inline. We're promoting that exact same spec to workspace level so executor-server can pick it up.)

- [ ] **Step 1.3: Switch executor-cli to workspace clap**

Edit `executor/crates/executor-cli/Cargo.toml`. Find the line `clap = { version = "4", features = ["derive", "env"] }` and replace with:

```toml
clap = { workspace = true }
```

- [ ] **Step 1.4: Verify the workspace still builds**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | tail -10`
Expected: success, no errors. (Cargo.lock will update; that's fine — commit it with the rest in Step 1.5.)

- [ ] **Step 1.5: Commit**

```bash
git add executor/Cargo.toml executor/crates/executor-cli/Cargo.toml executor/Cargo.lock
git commit -m "build(executor): promote clap to workspace dep for PR-C1 server CLI

executor-server gains --mode/--base flags in PR-C1; lifting clap to
workspace deps keeps the version pin aligned with executor-cli."
```

---

## Task 2: Add `HlError::UnknownSymbol` variant

**Files:**
- Modify: `executor/crates/executor-hl/src/errors.rs`

- [ ] **Step 2.1: Add the variant**

Edit `executor/crates/executor-hl/src/errors.rs`. After `ActionFormat(String)` (around line 26-27) and before the `Exchange { ... }` variant, insert:

```rust
    #[error("unknown symbol (not in MetaCache): {0}")]
    UnknownSymbol(executor_core::symbol::Symbol),
```

The `Symbol` type already implements `Display` (PR-A: `Symbol(String)` with `impl fmt::Display`). The `thiserror` `#[error("...{0}")]` will use that impl correctly.

- [ ] **Step 2.2: Verify the crate builds**

Run: `cd executor && cargo build -p executor-hl 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 2.3: Verify nothing else broke**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | tail -5`
Expected: clean (the new variant is additive; downstream code that exhaustively matches `HlError` would break, but the codebase doesn't do that — verify by grep if uncertain).

- [ ] **Step 2.4: Commit**

```bash
git add executor/crates/executor-hl/src/errors.rs
git commit -m "feat(executor-hl): HlError::UnknownSymbol(Symbol) for PR-C1 MetaCache misses

Per Gemini deep Q7: typed variant beats string-matching ActionFormat.
Drop site decisions (skip + log vs abort batch) live at the call site
in place_orders/cancel_orders."
```

---

## Task 3: Create `MetaCache` module

**Files:**
- Create: `executor/crates/executor-hl/src/meta.rs`
- Modify: `executor/crates/executor-hl/src/lib.rs`

- [ ] **Step 3.1: Create `meta.rs` with the struct and `build` + `resolve`**

Create `executor/crates/executor-hl/src/meta.rs`:

```rust
//! HL universe symbol → asset index cache.
//!
//! Built once at startup from `/info meta` endpoint(s). HL universe additions
//! (new coin listings) require process restart — explicit, fail-safe operation
//! per Gemini deep review (PR-C1, Q2).
//!
//! HIP-3 dex symbols (e.g. `xyz:META`) are stored with the dex prefix as part
//! of the key; lookup uses `Symbol`'s string-equality semantics.

use crate::errors::HlError;
use crate::hl_client::HlClient;
use executor_core::symbol::Symbol;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct MetaCache {
    by_symbol: HashMap<Symbol, u32>,
}

impl MetaCache {
    /// Empty cache. Useful for `RealHlClient::bootstrap()` and unit tests
    /// that drive the UnknownSymbol path.
    pub fn empty() -> Self {
        Self {
            by_symbol: HashMap::new(),
        }
    }

    /// Build cache by calling `fetch_meta` for each requested dex.
    /// `dexes = &[None]` fetches the default perp dex only;
    /// `&[None, Some("xyz")]` fetches default + the `xyz` HIP-3 dex.
    /// Symbols from non-default dexes are stored with the `<dex>:<name>`
    /// prefix matching HL's wire convention.
    pub async fn build(client: &dyn HlClient, dexes: &[Option<&str>]) -> Result<Self, HlError> {
        let mut by_symbol = HashMap::new();
        for dex in dexes {
            let meta = client.fetch_meta(*dex).await?;
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

    /// Resolve a symbol; returns `Err(HlError::UnknownSymbol)` if absent.
    pub fn resolve(&self, symbol: &Symbol) -> Result<u32, HlError> {
        self.by_symbol
            .get(symbol)
            .copied()
            .ok_or_else(|| HlError::UnknownSymbol(symbol.clone()))
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn empty_cache_resolves_to_unknown_symbol() {
        let m = MetaCache::empty();
        let err = m.resolve(&Symbol::new("ETH")).unwrap_err();
        assert!(matches!(err, HlError::UnknownSymbol(_)));
    }

    #[test]
    fn manual_insert_then_resolve_roundtrips() {
        let mut by_symbol = HashMap::new();
        by_symbol.insert(Symbol::new("BTC"), 0u32);
        by_symbol.insert(Symbol::new("ETH"), 1u32);
        by_symbol.insert(Symbol::new("xyz:META"), 7u32);
        let m = MetaCache { by_symbol };
        assert_eq!(m.resolve(&Symbol::new("BTC")).unwrap(), 0);
        assert_eq!(m.resolve(&Symbol::new("ETH")).unwrap(), 1);
        assert_eq!(m.resolve(&Symbol::new("xyz:META")).unwrap(), 7);
        assert!(m.resolve(&Symbol::new("MISSING")).is_err());
        assert_eq!(m.len(), 3);
    }
}
```

Note: the test constructs `MetaCache { by_symbol }` directly — that requires the `by_symbol` field to be visible inside the test module. Since both are in `meta.rs`, `mod tests { use super::*; ... }` already has access. No `pub(crate)` needed.

- [ ] **Step 3.2: Add `pub mod meta;` to lib.rs**

Edit `executor/crates/executor-hl/src/lib.rs`. Find the existing `pub mod` declarations. Add `pub mod meta;` alphabetically (between `errors` and `rate_limiter` if those are the alphabetical neighbors; otherwise between whatever comes alphabetically before/after `meta`).

Read `lib.rs` first to confirm the right insertion point.

- [ ] **Step 3.3: Run the new tests**

Run: `cd executor && cargo test -p executor-hl meta::tests -- --nocapture`
Expected: 2 tests pass.

- [ ] **Step 3.4: Run all executor-hl tests**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: 22 unit (was 21) + 22 integration = 44 tests pass.

- [ ] **Step 3.5: Clippy**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 3.6: Commit**

```bash
git add executor/crates/executor-hl/src/meta.rs executor/crates/executor-hl/src/lib.rs
git commit -m "feat(executor-hl): MetaCache for symbol → asset resolution

Built once at startup via fetch_meta(); HIP-3 dexes get prefix in keys
(<dex>:<name>). resolve() returns Err(UnknownSymbol) for misses; no
panic. Empty constructor for RealHlClient::bootstrap() and unit tests.

Per Gemini deep Q1/Q2: stored as field on RealHlClient (next task);
no DI to algo crate; Symbol(String) HashMap key (no enum gymnastics)."
```

---

## Task 4: RealHlClient `bootstrap` + `with_meta` + `resolve_asset`

**Files:**
- Modify: `executor/crates/executor-hl/src/hl_client.rs`

This task only modifies `RealHlClient`'s constructor surface and adds the resolver helper; it does NOT yet change `place_orders` / `cancel_orders` (Task 6 does that).

- [ ] **Step 4.1: Add `meta` field and bootstrap/with_meta constructors**

Edit `executor/crates/executor-hl/src/hl_client.rs`. Find the `RealHlClient` struct definition. Read the surrounding context first (around lines 380-420 based on PR-B2a; run `grep -n "pub struct RealHlClient" executor/crates/executor-hl/src/hl_client.rs` to find the exact line).

Modify the struct to add the `meta` field:

```rust
pub struct RealHlClient {
    pub config: HlConfig,
    pub signer: Arc<dyn Signer>,
    pub rate_limiter: Arc<TokenBucket>,
    pub http: reqwest::Client,
    /// PR-C1: pre-built symbol → asset cache. `RealHlClient::bootstrap()`
    /// initializes this empty so `fetch_meta()` can be called to BUILD the
    /// cache; `with_meta()` upgrades it. After `with_meta`, this is the
    /// authoritative source for asset resolution in place/cancel.
    pub meta: Arc<crate::meta::MetaCache>,
}
```

Then update the existing `impl RealHlClient { ... }` block. The current `new(config, signer)` constructor is the right place to host both `bootstrap` and `with_meta`. Replace `new` with:

```rust
impl RealHlClient {
    /// Construct a client with an EMPTY MetaCache. Use this when you need
    /// to call `fetch_meta()` to build the real cache; afterward, call
    /// `with_meta()` to produce a production-ready client.
    pub fn bootstrap(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Some(std::time::Duration::from_secs(60)))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            config,
            signer,
            rate_limiter: Arc::new(TokenBucket::hyperliquid_default()),
            http,
            meta: Arc::new(crate::meta::MetaCache::empty()),
        }
    }

    /// Replace the MetaCache. Returns a new `RealHlClient` reusing the
    /// existing http/signer/rate_limiter. Call this after building the cache.
    pub fn with_meta(self, meta: Arc<crate::meta::MetaCache>) -> Self {
        Self { meta, ..self }
    }

    /// Backwards-compatible alias. Existing callers used `new(config, signer)`
    /// with the implicit assumption that meta would be filled in later.
    /// New code should use `bootstrap` + `with_meta` explicitly.
    pub fn new(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
        Self::bootstrap(config, signer)
    }

    /// Resolve a symbol to its asset index using the cached meta.
    /// Used by both `place_orders` and `cancel_orders` (Task 6).
    pub(crate) fn resolve_asset(&self, symbol: &executor_core::symbol::Symbol) -> Result<u32, HlError> {
        self.meta.resolve(symbol)
    }

    // ... existing post_info / post_exchange methods unchanged ...
}
```

(If `RealHlClient` doesn't currently have an `impl` block separate from the trait impl, the new methods go in a fresh `impl RealHlClient { ... }` block — keep `post_info`/`post_exchange` in their current block.)

- [ ] **Step 4.2: Verify the crate builds**

Run: `cd executor && cargo build -p executor-hl 2>&1 | tail -5`
Expected: clean. The trait impls `place_orders`/`cancel_orders` still use the old logic and don't yet call `resolve_asset` — that's Task 6.

- [ ] **Step 4.3: Run all executor-hl tests**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: same as before Task 3 + 2 new MetaCache tests = 44 tests pass.

- [ ] **Step 4.4: Clippy**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4.5: Commit**

```bash
git add executor/crates/executor-hl/src/hl_client.rs
git commit -m "feat(executor-hl): RealHlClient gains meta field + bootstrap/with_meta

- bootstrap(config, signer): empty MetaCache; for the chicken-and-egg
  case where fetch_meta is needed BEFORE meta is built.
- with_meta(meta): upgrade after build. Production code path.
- new(): backwards-compat alias for bootstrap.
- resolve_asset(symbol) -> Result<u32, HlError>: pub(crate) helper.

place/cancel still use the old (asset-from-intent) wire path; Task 6
flips them over once OrderIntent.asset is gone (Task 5)."
```

---

## Task 5: Remove `asset` from OrderIntent / CancelIntent

**Files:**
- Modify: `executor/crates/executor-core/src/intent.rs`

This task is the moment the workspace will fail to compile in many places. Tasks 6, 7, 8 fix each layer.

- [ ] **Step 5.1: Read `intent.rs` to confirm the exact field positions**

Run: `cd /home/o9oem/workspace/crypto/diff-old-new && grep -n "pub asset" executor/crates/executor-core/src/intent.rs`
Expected: 2 hits, one in OrderIntent and one in CancelIntent.

- [ ] **Step 5.2: Remove the OrderIntent.asset field**

Edit `executor/crates/executor-core/src/intent.rs`. Find the `OrderIntent` struct. Replace the field block with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub cloid: Cloid,
    pub symbol: Symbol,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
}
```

(Confirm by reading the existing struct; the order is `cloid, symbol, asset, side, px, sz, tif, reduce_only`. Remove the `asset` line and its doc comment if any.)

- [ ] **Step 5.3: Remove the CancelIntent.asset field**

In the same file, find the `CancelIntent` struct. Replace with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelIntent {
    pub symbol: Symbol,
    pub by_cloid: Option<Cloid>,
    pub by_oid: Option<OrderId>,
}
```

- [ ] **Step 5.4: Run cargo build to surface every broken site**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | grep -E "error\[|^  --> " | head -80`
Expected: ~33 `error[E0560]: struct OrderIntent has no field 'asset'` or `CancelIntent has no field 'asset'` errors. Note each `file:line` printed — these are the sites Tasks 7, 8, 10 will fix.

DO NOT commit yet; the workspace doesn't build. Continue to Task 6 / 7 / 8 in sequence.

---

## Task 6: Update wire conversion + place_orders + cancel_orders

**Files:**
- Modify: `executor/crates/executor-hl/src/eip712.rs`
- Modify: `executor/crates/executor-hl/src/hl_client.rs`

- [ ] **Step 6.1: Change `order_intent_to_wire` signature in eip712.rs**

Edit `executor/crates/executor-hl/src/eip712.rs`. Find the existing function:

```rust
pub fn order_intent_to_wire(intent: &OrderIntent) -> OrderWire {
    OrderWire {
        a: intent.asset,    // ← compile error after Task 5
        b: matches!(intent.side, Side::Long),
        // ...
    }
}
```

Replace with:

```rust
pub fn order_intent_to_wire(intent: &OrderIntent, asset: u32) -> OrderWire {
    OrderWire {
        a: asset,
        b: matches!(intent.side, Side::Long),
        p: format!("{}", intent.px),
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
        c: Some(format!("{}", intent.cloid)),
    }
}
```

- [ ] **Step 6.2: Add a `cancel_intent_to_wire` helper to eip712.rs**

PR-B2a built `CancelByCloidWire` inline inside `cancel_orders`. Extract to a dedicated helper for symmetry with `order_intent_to_wire`. Append to `eip712.rs`:

```rust
use executor_core::intent::CancelIntent;

/// Convert a `CancelIntent` to the HL wire shape.
/// `intent.by_cloid` MUST be Some — by_oid-only cancellations are rejected
/// at the call site in `RealHlClient::cancel_orders` before reaching here.
pub fn cancel_intent_to_wire(intent: &CancelIntent, asset: u32) -> CancelByCloidWire {
    CancelByCloidWire {
        asset,
        cloid: format!(
            "{}",
            intent
                .by_cloid
                .expect("cancel_intent_to_wire: by_cloid must be Some — caller MUST validate")
        ),
    }
}
```

- [ ] **Step 6.3: Rewrite `RealHlClient::place_orders` to resolve assets and preserve order**

Edit `executor/crates/executor-hl/src/hl_client.rs`. Find the existing `async fn place_orders` in `impl HlClient for RealHlClient`. Replace the body (everything between the function's `{` and matching `}`) with:

```rust
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }

        let weight = 1 + (orders.len() as u32 / 40);
        let _wait = self.rate_limiter.acquire(weight).await;

        // Resolve each intent's asset; collect resolved (idx, wire) pairs and
        // immediately-rejected (idx, error_response) pairs so the final output
        // matches the input order exactly. UnknownSymbol drops a single order;
        // other Err variants abort the whole batch.
        let mut responses: Vec<Option<OrderResponse>> = (0..orders.len()).map(|_| None).collect();
        let mut wires_with_idx: Vec<(usize, crate::eip712::OrderWire)> = Vec::with_capacity(orders.len());
        for (i, intent) in orders.iter().enumerate() {
            match self.resolve_asset(&intent.symbol) {
                Ok(asset) => {
                    wires_with_idx.push((i, crate::eip712::order_intent_to_wire(intent, asset)));
                }
                Err(HlError::UnknownSymbol(sym)) => {
                    tracing::error!(symbol = %sym, "place_orders: unknown symbol; dropping");
                    responses[i] = Some(OrderResponse {
                        cloid: intent.cloid,
                        oid: None,
                        status: "error".into(),
                        error: Some(format!("unknown symbol: {sym}")),
                    });
                }
                Err(e) => return Err(e),
            }
        }

        if wires_with_idx.is_empty() {
            // Every order dropped (UnknownSymbol). Skip HTTP entirely.
            return Ok(responses.into_iter().flatten().collect());
        }

        let order_wires: Vec<crate::eip712::OrderWire> =
            wires_with_idx.iter().map(|(_, w)| w.clone()).collect();
        let action = crate::eip712::OrderAction {
            action_type: "order".into(),
            orders: order_wires,
            grouping: "na".into(),
        };
        let action_value = serde_json::to_value(&action)
            .map_err(|e| HlError::ActionFormat(format!("order serialize: {e}")))?;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();

        let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

        let body = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": sig,
            "vaultAddress": serde_json::Value::Null,
        });

        let resp_text = self.post_exchange(&body).await?;

        // Parse only the orders that were actually sent. We need to feed
        // parse_exchange_response a slice of OrderIntents matching the
        // wires_with_idx order; build it.
        let sent_intents: Vec<&OrderIntent> = wires_with_idx
            .iter()
            .map(|(i, _)| &orders[*i])
            .collect();
        // parse_exchange_response takes &[OrderIntent], not &[&OrderIntent].
        // Clone the references' data into a temporary Vec<OrderIntent>.
        let sent_intents_owned: Vec<OrderIntent> =
            sent_intents.iter().map(|r| (*r).clone()).collect();

        let parsed = parse_exchange_response(&resp_text, &sent_intents_owned)?;

        // Slot parsed responses back into the right indices.
        for ((i, _), resp) in wires_with_idx.iter().zip(parsed) {
            responses[*i] = Some(resp);
        }

        Ok(responses.into_iter().flatten().collect())
    }
```

Note on the trade-off: `Vec<OrderIntent>` cloning to feed `parse_exchange_response` is a small allocation cost. The cleaner long-term refactor is to change `parse_exchange_response`'s signature to accept `&[Cloid]` (it only uses cloid out of OrderIntent). That refactor is mentioned in the spec §5.1 but is out of scope for PR-C1; do it if a follow-up Gemini review demands it.

- [ ] **Step 6.4: Rewrite `RealHlClient::cancel_orders` for the same resolve-and-preserve-order pattern**

Find `async fn cancel_orders` in the same impl block. Replace the body with:

```rust
    async fn cancel_orders(
        &self,
        cancels: &[CancelIntent],
    ) -> Result<Vec<OrderResponse>, HlError> {
        if cancels.is_empty() {
            return Ok(Vec::new());
        }

        let weight = 1 + (cancels.len() as u32 / 40);
        let _wait = self.rate_limiter.acquire(weight).await;

        // Validate by_cloid presence and resolve asset per intent. Same
        // index-preservation pattern as place_orders.
        let mut responses: Vec<Option<OrderResponse>> = (0..cancels.len()).map(|_| None).collect();
        let mut wires_with_idx: Vec<(usize, crate::eip712::CancelByCloidWire)> = Vec::with_capacity(cancels.len());
        for (i, c) in cancels.iter().enumerate() {
            // by_oid-only is still rejected (PR-B2a contract; PR-B2b
            // emergency_stop fix accepts by_cloid + by_oid, using cloid).
            let cloid = match c.by_cloid {
                Some(cl) => cl,
                None => {
                    return Err(HlError::ActionFormat(
                        "by_oid-only cancel not supported in PR-B2a; CancelIntent must \
                         include by_cloid (PR-B2b will add by_oid path)"
                            .into(),
                    ));
                }
            };
            match self.resolve_asset(&c.symbol) {
                Ok(asset) => {
                    wires_with_idx.push((
                        i,
                        crate::eip712::CancelByCloidWire {
                            asset,
                            cloid: format!("{cloid}"),
                        },
                    ));
                }
                Err(HlError::UnknownSymbol(sym)) => {
                    tracing::error!(symbol = %sym, "cancel_orders: unknown symbol; dropping");
                    responses[i] = Some(OrderResponse {
                        cloid,
                        oid: None,
                        status: "error".into(),
                        error: Some(format!("unknown symbol: {sym}")),
                    });
                }
                Err(e) => return Err(e),
            }
        }

        if wires_with_idx.is_empty() {
            return Ok(responses.into_iter().flatten().collect());
        }

        let cancel_wires: Vec<crate::eip712::CancelByCloidWire> =
            wires_with_idx.iter().map(|(_, w)| w.clone()).collect();
        let action = crate::eip712::CancelByCloidAction {
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
            "vaultAddress": serde_json::Value::Null,
        });

        let resp_text = self.post_exchange(&body).await?;

        let sent_cancels_owned: Vec<CancelIntent> = wires_with_idx
            .iter()
            .map(|(i, _)| cancels[*i].clone())
            .collect();
        let parsed = parse_cancel_response(&resp_text, &sent_cancels_owned)?;

        for ((i, _), resp) in wires_with_idx.iter().zip(parsed) {
            responses[*i] = Some(resp);
        }

        Ok(responses.into_iter().flatten().collect())
    }
```

- [ ] **Step 6.5: Update `MockHlClient` to compile**

`MockHlClient::place_orders` currently constructs `OrderResponse` from `OrderIntent` and doesn't read `intent.asset`, so removing the field shouldn't break it. But the mock might have a synthetic `OrderWire` somewhere — verify.

Run: `cd executor && grep -nA20 "impl HlClient for MockHlClient" executor/crates/executor-hl/src/hl_client.rs | head -50`

Expected: the mock's `place_orders` returns `OrderResponse` directly without invoking `order_intent_to_wire`. If so, no MockHlClient changes needed for Task 6.

If the mock DOES invoke `order_intent_to_wire(intent)` (PR-B2a inline), update it to `order_intent_to_wire(intent, 1)` — the literal `1` is fine for mock since the asset value is never sent over the wire.

- [ ] **Step 6.6: Build the executor-hl crate to surface remaining errors**

Run: `cd executor && cargo build -p executor-hl --all-targets 2>&1 | grep -E "error\[" | head -20`
Expected: still 5 test-fixture-site errors in `batch_sender.rs`, `hl_client.rs` (test mod), `ws_state.rs`, `eip712.rs` (test), `place_cancel_mock.rs` — those are Task 8.

- [ ] **Step 6.7: Do NOT commit yet — proceed to Task 7**

Tasks 5+6 form one atomic logical unit (compiler-driven refactor). Hold the commit until Task 8 completes the executor-hl crate. Server-level callers (Task 9) and the algo crate (Task 10) follow.

Actually — to keep commits reviewable, we WILL commit after each Task even though intermediate states leave the workspace broken on the branch. The squash merge at PR time produces one logical commit on develop. So:

- [ ] **Step 6.8: Commit**

```bash
git add executor/crates/executor-hl/src/eip712.rs executor/crates/executor-hl/src/hl_client.rs
git commit -m "refactor(executor-hl): place/cancel resolve via MetaCache; preserve input order

- order_intent_to_wire(intent, asset): asset now an explicit arg.
- cancel_intent_to_wire(intent, asset): symmetric extraction from
  inline PR-B2a code.
- place_orders / cancel_orders use Vec<Option<OrderResponse>> to keep
  output order aligned with input even when UnknownSymbol drops some.
- by_oid-only cancel still rejected; by_cloid (with or without by_oid)
  goes through normally per PR-B2b emergency_stop fix.

Workspace still broken at this commit (intent.asset removal in Task 5
left ~25 caller errors in algo crate + test fixtures + executor-server;
Tasks 7-10 fix them)."
```

---

## Task 7: Fix executor-hl test fixture call sites

**Files:**
- Modify: `executor/crates/executor-hl/src/batch_sender.rs`
- Modify: `executor/crates/executor-hl/src/hl_client.rs` (test mod)
- Modify: `executor/crates/executor-hl/src/ws_state.rs`
- Modify: `executor/crates/executor-hl/src/eip712.rs` (test mod)

- [ ] **Step 7.1: Remove `asset: 0,` from batch_sender.rs callers**

Find lines 244 and 290 (or use grep): `grep -n "asset: 0," executor/crates/executor-hl/src/batch_sender.rs`. Each should be inside a `#[cfg(test)] mod tests` block. Delete each `asset: 0,` line entirely (no replacement).

- [ ] **Step 7.2: Remove `asset: 0,` from hl_client.rs test fixtures**

Find lines 779 and 792 (or grep): `grep -n "asset: 0," executor/crates/executor-hl/src/hl_client.rs`. Both are in the test module. Delete each line.

- [ ] **Step 7.3: Remove `asset: 0,` from ws_state.rs test**

Find line 225 (grep): `grep -n "asset: 0," executor/crates/executor-hl/src/ws_state.rs`. Delete the line.

- [ ] **Step 7.4: Remove `asset: 1,` from eip712.rs test**

Find line 269 (grep): `grep -n "asset: 1," executor/crates/executor-hl/src/eip712.rs`. Delete the line.

- [ ] **Step 7.5: Build executor-hl tests**

Run: `cd executor && cargo build -p executor-hl --all-targets 2>&1 | grep -E "error\[" | head -10`
Expected: zero `error[E0560]` for OrderIntent/CancelIntent.asset within executor-hl. If any remain, grep again and fix.

- [ ] **Step 7.6: Run the executor-hl test suite**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: all tests pass, including the new `meta::tests` count.

The integration test file `tests/place_cancel_mock.rs` still has `asset: 1,` literals (PR-B2a). Those become Task 8.

- [ ] **Step 7.7: Commit**

```bash
git add executor/crates/executor-hl/src/batch_sender.rs \
        executor/crates/executor-hl/src/hl_client.rs \
        executor/crates/executor-hl/src/ws_state.rs \
        executor/crates/executor-hl/src/eip712.rs
git commit -m "refactor(executor-hl tests): drop asset literals from fixture sites

5 sites under #[cfg(test)] modules. Mock backend doesn't validate the
field (PR-C1 spec §4.5), so deletion is sufficient — no replacement
needed."
```

---

## Task 8: Fix executor-hl integration tests

**Files:**
- Modify: `executor/crates/executor-hl/tests/place_cancel_mock.rs`
- Modify: `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs`

- [ ] **Step 8.1: Remove `asset: 1,` from place_cancel_mock.rs**

Run: `cd /home/o9oem/workspace/crypto/diff-old-new && grep -n "asset: 1," executor/crates/executor-hl/tests/place_cancel_mock.rs`
Expected: 4 hits.

For each, edit and delete the `asset: 1,` line.

- [ ] **Step 8.2: Add new test for UnknownSymbol path**

Append to `executor/crates/executor-hl/tests/place_cancel_mock.rs` (after the last existing test, before the file's end):

```rust
#[tokio::test]
async fn place_orders_unknown_symbol_returns_error_response_no_http() {
    // RealHlClient::bootstrap() has an EMPTY MetaCache. Any symbol → UnknownSymbol.
    // Verify (a) the response is an error with "unknown symbol" message,
    // (b) no HTTP request is made (the mock server gets zero hits).
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap(),
    );
    let server = mockito::Server::new_async().await;
    // Note: NO `.mock("POST", "/exchange")` — if the code calls /exchange
    // mockito will return 501 and the test will fail in a useful way.

    let config = HlConfig {
        info_url: format!("{}/info", server.url()),
        exchange_url: format!("{}/exchange", server.url()),
        ws_url: "ws://unused".into(),
    };
    let client = RealHlClient::bootstrap(config, signer);
    let intent = make_order_intent();
    let cloid = intent.cloid;
    let resp = client.place_orders(&[intent]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "error");
    assert_eq!(resp[0].cloid, cloid);
    assert!(
        resp[0]
            .error
            .as_deref()
            .unwrap()
            .contains("unknown symbol"),
        "expected 'unknown symbol' in error: {:?}",
        resp[0].error
    );
}
```

This test relies on `make_order_intent()` already existing in the file from PR-B2a. After Task 8.1, that helper no longer constructs an intent with an `asset` field, so it returns the simplified PR-C1 shape automatically.

- [ ] **Step 8.3: Update `live_mainnet_place_cancel.rs` — remove `asset: eth_idx,` and the lookup**

Find the test in `executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs`:

Run: `cd /home/o9oem/workspace/crypto/diff-old-new && grep -n "asset:\|eth_idx" executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs`
Expected hits include `asset: eth_idx,` in OrderIntent/CancelIntent constructors and `let eth_idx = ...` resolution code.

Apply two edits:

**Edit A**: in the OrderIntent and CancelIntent constructors, delete the `asset: eth_idx,` line.

**Edit B**: the existing `fetch_meta()` call + `eth_idx` derivation. Keep the `fetch_meta()` call and `let eth_idx = ...` for the eprintln diagnostic, but mark `eth_idx` as `_eth_idx` since it's unused after the asset removal:

```rust
    // === ETH index resolve via fetch_meta (diagnostic only post-PR-C1) ===
    let meta = client.fetch_meta(None).await.expect("fetch meta");
    let _eth_idx = meta
        .universe
        .iter()
        .position(|u| u.name == "ETH")
        .expect("ETH not in default perp universe") as u32;
    eprintln!(
        "ETH found in meta (asset index assigned via MetaCache inside RealHlClient post-PR-C1)"
    );
```

This preserves the diagnostic value (proves meta is fetchable + ETH is present) without coupling the test to the index value.

The `RealHlClient` constructed in the test (`make_client()`) currently uses `RealHlClient::new()`, which in PR-C1 still works (alias to `bootstrap`). But `bootstrap` has an EMPTY MetaCache, so calling `place_orders` with `Symbol::new("ETH")` would now drop with UnknownSymbol — the test would fail.

The test must build the MetaCache before placing. Update `make_client()` to do the bootstrap + with_meta dance:

```rust
async fn make_client() -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(agent_pk_secret(), true /* is_mainnet */)
            .expect("Eip712AgentSigner::from_secret failed; HL_AGENT_PK malformed?"),
    );
    let bootstrap = RealHlClient::bootstrap(HlConfig::mainnet(), signer);
    let meta = std::sync::Arc::new(
        executor_hl::meta::MetaCache::build(&bootstrap, &[None])
            .await
            .expect("MetaCache::build failed at PR-B2b live test setup"),
    );
    bootstrap.with_meta(meta)
}
```

(Note `make_client` becomes `async`; update the call site `let client = make_client();` to `let client = make_client().await;` in the test body. There should be one such call.)

- [ ] **Step 8.4: Verify the live test still compiles (without --features live, which won't run it)**

Run: `cd executor && cargo build -p executor-hl --tests --features live 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 8.5: Run the mock test suite**

Run: `cd executor && cargo test -p executor-hl --test place_cancel_mock 2>&1 | tail -15`
Expected: 9 tests pass (was 8; +1 from the new UnknownSymbol test in Step 8.2).

- [ ] **Step 8.6: Commit**

```bash
git add executor/crates/executor-hl/tests/place_cancel_mock.rs \
        executor/crates/executor-hl/tests/live_mainnet_place_cancel.rs
git commit -m "test(executor-hl): drop asset literals; add UnknownSymbol mock test

place_cancel_mock: 4 fixture sites lose `asset: 1,`; new test
place_orders_unknown_symbol_returns_error_response_no_http exercises
the empty-MetaCache drop path (RealHlClient::bootstrap → place →
error response, no HTTP).

live_mainnet_place_cancel: drop `asset: eth_idx,` from OrderIntent/
CancelIntent constructors. fetch_meta() retained for diagnostic
eprintln; the actual asset is now resolved inside RealHlClient via
MetaCache (built in make_client via bootstrap + with_meta)."
```

---

## Task 9: Fix executor-server caller (routes.rs emergency_stop)

**Files:**
- Modify: `executor/crates/executor-server/src/routes.rs`

- [ ] **Step 9.1: Remove `asset: 0,` from emergency_stop**

Run: `cd /home/o9oem/workspace/crypto/diff-old-new && grep -n "asset: 0," executor/crates/executor-server/src/routes.rs`
Expected: 1 hit, near line 266 (PR-B2a).

Delete the `asset: 0,` line.

- [ ] **Step 9.2: Verify executor-server builds**

Run: `cd executor && cargo build -p executor-server --all-targets 2>&1 | grep -E "error\[" | head -10`
Expected: zero errors.

- [ ] **Step 9.3: Run executor-server tests**

Run: `cd executor && cargo test -p executor-server 2>&1 | grep "test result" | tail -10`
Expected: all pass (8 unit + 10 integration = 18 tests, same as PR-B2a baseline).

- [ ] **Step 9.4: Commit**

```bash
git add executor/crates/executor-server/src/routes.rs
git commit -m "refactor(executor-server/routes): drop asset:0 from emergency_stop

PR-C1: CancelIntent.asset is gone. emergency_stop builds CancelIntent
with by_cloid; the asset is resolved inside RealHlClient::cancel_orders
via the MetaCache (which routes are constructed against)."
```

---

## Task 10: Fix executor-algo runtime callers

**Files:**
- Modify: `executor/crates/executor-algo/src/market.rs` (1 caller)
- Modify: `executor/crates/executor-algo/src/market_make.rs` (3 callers)
- Modify: `executor/crates/executor-algo/src/passive_follow.rs` (5 callers)
- Modify: `executor/crates/executor-algo/src/twap.rs` (6 callers)

15 sites total in the algo runtime crate (the spec says "16" because passive_follow grep returned 5 + market_make 3 + twap 6 + market 1 = 15 in production code; PR-B2a actually placed 16 across the algo crate — verify with grep).

- [ ] **Step 10.1: Confirm exact site count**

Run:
```bash
cd /home/o9oem/workspace/crypto/diff-old-new
grep -rn "asset: 0," executor/crates/executor-algo/ --include="*.rs"
grep -rn "// TODO(PR-B2b)" executor/crates/executor-algo/ --include="*.rs"
```
Expected: ~16 hits each. The TODO comments live one line above each `asset: 0,`.

- [ ] **Step 10.2: For each file, delete both the TODO comment and the asset line**

For each algo file, use Edit (one file at a time, multiple non-replace_all edits per file with enough surrounding context to disambiguate):

Pattern to remove:
```rust
                    // TODO(PR-B2b): resolve via meta cache (currently placeholder)
                    asset: 0,
```

Replacement: empty (delete both lines).

Files: `market.rs` (1 site), `market_make.rs` (3 sites), `passive_follow.rs` (5 sites), `twap.rs` (6 sites).

- [ ] **Step 10.3: Verify the algo crate builds**

Run: `cd executor && cargo build -p executor-algo --all-targets 2>&1 | grep -E "error\[" | head -10`
Expected: zero errors.

- [ ] **Step 10.4: Run the algo test suite**

Run: `cd executor && cargo test -p executor-algo 2>&1 | grep "test result" | tail -5`
Expected: 56 tests pass (PR-B2a baseline; algo tests use MockHlClient which doesn't care about asset).

- [ ] **Step 10.5: Verify NO `// TODO(PR-B2b)` remains anywhere in the workspace**

Run: `grep -rn "TODO(PR-B2b)" executor/crates/ --include="*.rs"`
Expected: empty output.

- [ ] **Step 10.6: Commit**

```bash
git add executor/crates/executor-algo/
git commit -m "refactor(executor-algo): drop asset:0 placeholders + TODO(PR-B2b)

15 OrderIntent / CancelIntent construction sites across market,
market_make, passive_follow, twap. Each had:
  // TODO(PR-B2b): resolve via meta cache (currently placeholder)
  asset: 0,

Both lines removed. The asset is now resolved inside
RealHlClient::place_orders/cancel_orders via the MetaCache built
at server startup (PR-C1 §4.7).

Algo runtime no longer needs to know HL universe indices — it just
emits OrderIntent { symbol, side, ... } and trusts the wire layer."
```

---

## Task 11: executor-server main.rs clap + real mode bootstrap

**Files:**
- Modify: `executor/crates/executor-server/Cargo.toml`
- Modify: `executor/crates/executor-server/src/main.rs`

- [ ] **Step 11.1: Add clap and secrecy to executor-server deps**

Edit `executor/crates/executor-server/Cargo.toml`. In `[dependencies]` add:

```toml
clap = { workspace = true }
secrecy = { workspace = true }
```

(Both are already in workspace deps; just expose them to executor-server.)

- [ ] **Step 11.2: Read the current main.rs**

Run: `cat /home/o9oem/workspace/crypto/diff-old-new/executor/crates/executor-server/src/main.rs`

Note the current shape: it constructs `MockHlClient` + `MockSigner` + `BatchSender` + `ServerState` + `axum` server.

- [ ] **Step 11.3: Replace main.rs with the clap-driven version**

Replace the entire body of `executor/crates/executor-server/src/main.rs` with:

```rust
//! executor-server: axum REST + WS bin (PR-7, PR-C1).
//!
//! Mode selection (clap):
//! - `--mode mock` (default): MockHlClient + MockSigner; runs without keys.
//! - `--mode real`: RealHlClient + Eip712AgentSigner; needs HL_AGENT_PK env.
//!
//! `--base mainnet|testnet` selects the HL endpoint. Only consulted in real mode.
//!
//! Real mode does the bootstrap-then-upgrade dance:
//!   1. Construct an empty-MetaCache RealHlClient.
//!   2. Call MetaCache::build(client, &[None]) — fetches /info meta (default dex).
//!   3. Replace via with_meta() to get a production client.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use executor_core::state::AppState;
use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
use executor_hl::hl_client::{HlClient, HlConfig, MockHlClient, RealHlClient};
use executor_hl::meta::MetaCache;
use executor_hl::signer::{Eip712AgentSigner, MockSigner, Signer};
use executor_server::{build_app, ServerState};
use secrecy::SecretString;

#[derive(Parser, Debug)]
#[command(name = "executor-server", version)]
struct Args {
    /// Backend mode: `mock` for CI/test, `real` for mainnet/testnet.
    #[arg(long, default_value = "mock")]
    mode: Mode,

    /// HL endpoint base. Only relevant in `--mode real`.
    #[arg(long, default_value = "mainnet")]
    base: Base,

    /// Bind address (host:port).
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,executor_=debug")),
        )
        .with_target(true)
        .compact()
        .init();

    let args = Args::parse();

    let app_state = Arc::new(AppState::new());

    let (hl_client, signer): (Arc<dyn HlClient>, Arc<dyn Signer>) = match args.mode {
        Mode::Mock => {
            tracing::info!("starting in mock mode");
            let mock_hl: Arc<dyn HlClient> = Arc::new(MockHlClient::new());
            let mock_signer: Arc<dyn Signer> = Arc::new(MockSigner::new());
            (mock_hl, mock_signer)
        }
        Mode::Real => {
            let is_mainnet = matches!(args.base, Base::Mainnet);
            tracing::info!(?args.base, "starting in real mode");
            let config = match args.base {
                Base::Mainnet => HlConfig::mainnet(),
                Base::Testnet => HlConfig::testnet(),
            };
            let pk = std::env::var("HL_AGENT_PK")
                .context("HL_AGENT_PK env required for --mode real (run scripts/load-env.sh)")?;
            let signer: Arc<dyn Signer> = Arc::new(
                Eip712AgentSigner::from_secret(SecretString::new(pk.into()), is_mainnet)
                    .context("Eip712AgentSigner::from_secret failed")?,
            );
            let bootstrap = RealHlClient::bootstrap(config, signer.clone());
            let meta = Arc::new(
                MetaCache::build(&bootstrap, &[None])
                    .await
                    .context("MetaCache::build (default dex) failed at startup")?,
            );
            tracing::info!(symbols = meta.len(), "MetaCache built (default dex)");
            let real_client: Arc<dyn HlClient> = Arc::new(bootstrap.with_meta(meta));
            (real_client, signer)
        }
    };

    let (batch_sender, batch_handle) = spawn_batch_sender(
        hl_client.clone(),
        BatchSenderConfig {
            flush_interval: Duration::from_millis(100),
            max_batch_size: 50,
        },
    );

    let state = Arc::new(ServerState::new(
        app_state,
        hl_client,
        signer,
        batch_sender,
        batch_handle,
    ));

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    tracing::info!(addr = %args.bind, "executor-server listening");
    axum::serve(listener, app)
        .await
        .context("axum::serve")?;
    Ok(())
}
```

Note: the existing main.rs's bind call may differ slightly from the above (e.g. use of `axum::Server::bind` vs `axum::serve(listener, ...)`). Read the current code first and match its existing bind pattern; the only meaningful changes here are (a) clap-driven `args.mode/args.base`, (b) the `match args.mode { ... }` branch, (c) replacing direct construction of `MockHlClient`/`MockSigner` with conditional construction.

If the existing main has any post-shutdown handling (e.g. `batch_handle.abort()`), preserve it.

- [ ] **Step 11.4: Build the executor-server crate**

Run: `cd executor && cargo build -p executor-server 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 11.5: Smoke-test mock mode startup**

Run: `cd executor && cargo run -p executor-server -- --mode mock 2>&1 | head -20 &`
Wait 2 seconds for it to bind.
Then: `curl -s http://localhost:8085/v1/health | head`
Expected: a JSON health response (the existing /v1/health endpoint).
Then: `pkill -f executor-server`.

(If you can't run a live binary in your environment, skip the curl test and rely on `cargo build` + the existing `cargo test -p executor-server` integration tests for confirmation.)

- [ ] **Step 11.6: Run the executor-server test suite**

Run: `cd executor && cargo test -p executor-server 2>&1 | grep "test result" | tail -10`
Expected: 18 tests pass (PR-B2a baseline).

- [ ] **Step 11.7: Commit**

```bash
git add executor/crates/executor-server/Cargo.toml executor/crates/executor-server/src/main.rs
git commit -m "feat(executor-server): clap --mode mock|real + --base mainnet|testnet

Real mode does the bootstrap-then-upgrade dance: empty-MetaCache
client, call MetaCache::build(client, &[None]), then with_meta().
HL_AGENT_PK env required for real mode. Default mode is mock; CI
keeps running unchanged.

Per Gemini deep Q4: only --mode/--base added now; allowlist + size
cap flags are PR-C2."
```

---

## Task 12: Workspace verification + HANDOFF

**Files:**
- Modify: `docs/HANDOFF-2026-05-04.md`

- [ ] **Step 12.1: Workspace-wide build + test + clippy + fmt**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new/executor
cargo build --workspace --all-targets 2>&1 | tail -5
cargo test --workspace 2>&1 | grep "test result" | tail -10
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --all -- --check 2>&1 | tail -5
```

Expected:
- Build: clean
- Tests: 142 (PR-B2a baseline) + 2 (MetaCache unit) + 1 (UnknownSymbol mock) = 145 pass
- Clippy: clean
- Fmt: clean (run `cargo fmt --all` if not, then re-check)

- [ ] **Step 12.2: Confirm zero `asset:` literals in OrderIntent/CancelIntent constructions**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
grep -rn "asset: 0,\|asset: 1," executor/crates/ --include="*.rs"
```
Expected: zero hits (only `eip712.rs::CancelByCloidWire { asset: ... }` should still have `asset:` since that's the wire struct and keeps the field; verify any remaining hits are in wire struct definitions, not Intent constructions).

- [ ] **Step 12.3: Confirm zero `// TODO(PR-B2b)` remains**

```bash
grep -rn "TODO(PR-B2b)" executor/crates/ --include="*.rs"
```
Expected: zero hits.

- [ ] **Step 12.4: Confirm `OrderIntent.asset` is gone**

```bash
grep -rn "OrderIntent.*asset\|CancelIntent.*asset" executor/crates/ --include="*.rs" | grep -v "wire" | head
```
Expected: zero hits in non-wire code (the wire structs `OrderWire { a }` and `CancelByCloidWire { asset }` are kept since they are the wire representation).

- [ ] **Step 12.5: Update HANDOFF doc**

Edit `docs/HANDOFF-2026-05-04.md`. Find the existing PR-B2b 完了 subsection. Append AFTER it:

```
#### 2026-05-05 PR-C1 完了

- `OrderIntent` / `CancelIntent` から `asset: u32` field 完全削除 (PR-B2a で追加した field を撤回, Gemini deep Q3 の Leaky Abstraction 解消)
- `executor-hl::meta::MetaCache` 新設. RealHlClient struct field として保持, bootstrap → fetch_meta → with_meta の 2 段構築
- `HlError::UnknownSymbol(Symbol)` 新規 variant. resolve 失敗時は per-order error response で skip + log (panic 禁止, batch 全体 abort もしない)
- `RealHlClient::place_orders` / `cancel_orders` で内部 resolve, `Vec<Option<OrderResponse>>` で input order 保持
- algo runtime 16 caller の `// TODO(PR-B2b)` placeholder 全消去
- `executor-server` に clap 導入: `--mode mock|real` (default mock), `--base mainnet|testnet` (default mainnet)
- 実 mode は `HL_AGENT_PK` env 必須 (`source scripts/load-env.sh` 経由)
- workspace 145 tests pass (PR-B2a 142 + MetaCache 2 + UnknownSymbol mock 1)
- Gemini deep review (gemini-3.1-pro-preview, 2026-05-05) 8 項目を全採用
- **PR-C2 へ持ち越し**: symbol allowlist (`--mainnet-allow-symbols ETH`), size cap (`--mainnet-max-notional-usd 20`), middleware による server 内蔵 gate
```

- [ ] **Step 12.6: Run the local CI script**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
bash scripts/check_ci_local.sh 2>&1 | tail -20
```
Expected: green ("All CI checks passed locally").

- [ ] **Step 12.7: Commit HANDOFF**

```bash
git add docs/HANDOFF-2026-05-04.md
git commit -m "docs: HANDOFF — PR-C1 (MetaCache + executor-server real mode) merged"
```

---

## Task 13: Gemini deep review + PR + merge

**Files:** (review may produce additional commits)

- [ ] **Step 13.1: Generate code-only diff**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git diff develop...HEAD -- \
  executor/crates/executor-core/src/intent.rs \
  executor/crates/executor-hl/src/ \
  executor/crates/executor-hl/tests/ \
  executor/crates/executor-algo/src/ \
  executor/crates/executor-server/src/ \
  executor/crates/executor-server/Cargo.toml \
  executor/crates/executor-cli/Cargo.toml \
  executor/Cargo.toml \
  > /tmp/pr-c1-diff.patch
wc -l /tmp/pr-c1-diff.patch
wc -c /tmp/pr-c1-diff.patch | awk '{printf "%.1f KB\n", $1/1024}'
```

- [ ] **Step 13.2: Run Gemini deep review (with flash fallback per memory)**

```bash
{
  echo "PR-C1: MetaCache + executor-server real mode 切替."
  echo "Spec: docs/superpowers/specs/2026-05-05-pr-c1-meta-cache-and-real-mode-design.md"
  echo "Plan: docs/superpowers/plans/2026-05-05-pr-c1-meta-cache-and-real-mode-plan.md"
  echo
  echo "## 達成"
  echo "- OrderIntent / CancelIntent から asset: u32 完全削除 (PR-B2a で追加した field を撤回)."
  echo "- executor-hl::meta::MetaCache 新設, RealHlClient field として保持."
  echo "- bootstrap → fetch_meta → with_meta の 2 段構築 (chicken-and-egg 解決)."
  echo "- HlError::UnknownSymbol(Symbol) 新設. UnknownSymbol は per-order error response で skip + log."
  echo "- Vec<Option<OrderResponse>> で input order 保持."
  echo "- algo runtime 16 caller の // TODO(PR-B2b) 全消去."
  echo "- executor-server に clap 導入: --mode mock|real, --base mainnet|testnet."
  echo "- workspace 145 tests pass."
  echo
  echo "## 観点"
  echo "1. bootstrap → with_meta 2 段構築が本当に必要か, 単一の async build メソッドの方が clean か."
  echo "2. UnknownSymbol を skip + log で進める (batch 全体 abort しない) 設計が production trading で安全か."
  echo "3. Vec<Option<OrderResponse>> による order preservation, よりシンプルな書き方は?"
  echo "4. order_intent_to_wire(intent, asset) シグネチャ変更, 引数順序は妥当か."
  echo "5. parse_exchange_response 等の sent_intents_owned: Vec<OrderIntent> clone コスト. 将来の refactor 余地."
  echo "6. main.rs の env var (HL_AGENT_PK) 失敗時の fail-fast 動作."
  echo "7. mock mode と real mode のテスト戦略の対称性."
  echo "8. multi-dex 対応 (PR-C 以降) への extension point の妥当性."
  echo
  echo "## 期待するレビュー"
  echo "- MUST-FIX: 安全 / セキュリティ / logic 問題."
  echo "- SHOULD-FIX: PR-C2 までに直すべき設計問題."
  echo "- SUGGESTION: 将来検討."
  echo
  echo "## Diff (~$(wc -l < /tmp/pr-c1-diff.patch) lines, $(wc -c < /tmp/pr-c1-diff.patch | awk "{printf \"%.1f\", \$1/1024}") KB)"
  echo
  cat /tmp/pr-c1-diff.patch
} | ~/.claude/hooks/gemini-review.sh deep --timeout 240 2>&1 | tee /tmp/pr-c1-gemini-review.md | tail -150
```

If `gemini-review.sh deep` times out or returns "Both OAuth and API Key failed", fall back to flash:

```bash
# Same prompt, but use qa mode (flash-lite)
{ ... same prompt ... } | ~/.claude/hooks/gemini-review.sh qa --timeout 90 2>&1 | tee /tmp/pr-c1-gemini-review.md | tail -150
```

- [ ] **Step 13.3: Address review comments**

For each MUST-FIX:
1. Make the change.
2. Re-run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
3. Commit each fix as its own commit (`fix(executor-hl): <comment summary>`).

For SHOULD-FIX/SUGGESTION: defer to PR-C2 if larger; apply if quick + low risk.

- [ ] **Step 13.4: Push branch and open PR with `--base develop`**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git push -u origin feat/pr-c1-meta-cache-real-mode
gh pr create --base develop --title "feat(executor): PR-C1 — MetaCache + executor-server real mode 切替" --body "$(cat <<'EOF'
## Summary

Stage C step 1 (PR-C1) of the C-1 段階的検証 spec. Production-path infra fix:
removes a leaky abstraction (`OrderIntent.asset: u32`) added in PR-B2a, introduces
`MetaCache` for symbol→asset resolution inside `RealHlClient`, and adds clap-driven
`--mode mock|real` + `--base mainnet|testnet` to executor-server.

- `executor-hl::meta::MetaCache` new module; `RealHlClient::bootstrap()` → `MetaCache::build(&client, &[None])` → `RealHlClient::with_meta()` resolves the chicken-and-egg without making `RealHlClient::new()` async.
- `HlError::UnknownSymbol(Symbol)` typed variant; misses become per-order error responses (skip + log), no panic, no batch abort.
- `Vec<Option<OrderResponse>>` placeholder pattern keeps output order aligned with input across drops.
- 16 algo-runtime sites lose `// TODO(PR-B2b)` + `asset: 0,` placeholders. 5 test fixtures + 1 emergency_stop site lose their literal asset values too.
- New mock test: `place_orders_unknown_symbol_returns_error_response_no_http` exercises the empty-MetaCache drop path.
- live_mainnet_place_cancel test reworked to use `bootstrap → MetaCache::build → with_meta` instead of hardcoded `asset: eth_idx`.
- Decisions per Gemini deep review (gemini-3.1-pro-preview, 8 questions; full record in spec §3.2).

## Test plan

- [x] `cargo test --workspace` — 145 tests pass (was 142)
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [x] `cargo fmt --all -- --check` — clean
- [x] `bash scripts/check_ci_local.sh` — green
- [x] `grep -rn "asset: 0,\|asset: 1," executor/crates/ --include="*.rs"` — zero hits in Intent constructors (only wire struct definitions remain, as expected)
- [x] `grep -rn "TODO(PR-B2b)" executor/crates/ --include="*.rs"` — zero hits
- [ ] (post-merge, user-executed) `cargo test -p executor-hl --features live live_mainnet_place_cancel -- --nocapture --test-threads=1` — single round trip should still pass mainnet, now with MetaCache rather than hardcoded eth_idx
- [ ] (post-merge, user-executed) `cargo run -p executor-server -- --mode real --base mainnet` — should start, build MetaCache, log `symbols = N` (where N >= 230 for mainnet default dex), and bind

## Notes

- This PR has **no fee-incurring HL request paths** added; the only new HL traffic at startup in real mode is one `/info meta` call (weight 20 of 1200/min, trivial).
- Bootstrap with empty MetaCache exists ONLY to call `fetch_meta()` to BUILD the real one. After `with_meta()`, the bootstrap client should be dropped or replaced. The `new()` alias is preserved for backwards-compat.
- `UnknownSymbol` is a NON-fatal error path: production code that emits orders for valid symbols (i.e. all algorithms in the workspace today) is unaffected. The drop-and-log behavior is for defense in depth against future configuration mistakes.
- Multi-dex MetaCache build (`&[None, Some("xyz")]` etc.) is supported but not enabled by default in main.rs. PR-C2 may enable it.

## Deferred to PR-C2 / PR-C3 / PR-C4

- `--mainnet-allow-symbols ETH` flag + middleware gate (PR-C2).
- `--mainnet-max-notional-usd 20` flag + middleware gate (PR-C2).
- Server-side baseline-diff guard with auto emergency_stop (PR-C3).
- emergency_stop multi-symbol live test, e2e live test (PR-C4).
- Multi-dex universe in MetaCache::build (e.g. `&[None, Some("xyz")]`) — currently only default dex.
- Refactor `parse_exchange_response` / `parse_cancel_response` to take `&[Cloid]` instead of `&[OrderIntent]` to drop the clone in the new code path.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 13.5: Watch CI and merge if green**

```bash
gh pr checks <PR_NUMBER> --watch --interval 15
gh pr merge <PR_NUMBER> --squash --delete-branch
```

- [ ] **Step 13.6: Sync develop locally**

```bash
git checkout develop
git pull origin develop
git log --oneline -3
```

---

## Plan Self-Review

**Spec coverage:**

| Spec section | Plan task |
|---|---|
| §3.1 existing code state | All tasks reference current PR-B2b state |
| §3.2 Gemini Q1: MetaCache as RealHlClient field | Task 4 |
| §3.2 Gemini Q2: Symbol(String) maintained | No change required (verified in Task 3 test) |
| §3.2 Gemini Q3: enum Symbol skipped | Confirmed by absence (no enum task) |
| §3.2 Gemini Q4: --mode + --base only | Task 11 (no extra flags) |
| §3.2 Gemini Q5: resolve_asset shared by place + cancel | Task 4 (helper), Task 6 (uses) |
| §3.2 Gemini Q6: MockHlClient unchanged | Task 6 Step 6.5 (verify) |
| §3.2 Gemini Q7: HlError::UnknownSymbol | Task 2 |
| §3.2 Gemini Q8: 1 PR | All tasks in one branch |
| §4.1 MetaCache impl | Task 3 |
| §4.2 Intent.asset removal | Task 5 |
| §4.3 UnknownSymbol variant | Task 2 |
| §4.4 RealHlClient bootstrap/with_meta/resolve_asset | Task 4 |
| §4.5 MockHlClient unchanged | Task 6 Step 6.5 |
| §4.6 BatchSender no signature change | Task 7 (test fixtures only) |
| §4.7 main.rs clap | Task 11 |
| §4.8 21+ caller cleanup | Tasks 7, 8, 9, 10 |
| §4.9 file structure | Tasks 1-12 each touch the listed files |
| §4.10 mock test for UnknownSymbol | Task 8 Step 8.2 |
| §4.11 acceptance criteria | Task 12 + Task 13 |
| §5.1 Vec<Option<OrderResponse>> order preservation | Task 6 Step 6.3, 6.4 |
| §5.2 wire signature change | Task 6 Step 6.1, 6.2 |
| §5.3 multi-dex YAGNI | Task 11 (`&[None]` only) |
| §5.4 mainnet startup impact | Task 13.4 PR body notes |

No spec gaps.

**Placeholder scan:** No `TBD`, `implement later`, or `similar to Task N` patterns. Every code-changing step shows the exact code or the exact grep + delete operation. The `// TODO(PR-B2b)` comments removed in Task 10 are existing repo content being cleaned up, not plan placeholders.

**Type consistency:**
- `MetaCache` defined in Task 3 used in Task 4 (`Arc<MetaCache>` field) and Task 8 (test) and Task 11 (main.rs).
- `RealHlClient::bootstrap`, `with_meta`, `resolve_asset` defined in Task 4 used consistently in Task 6 and Task 11.
- `OrderIntent` field set `{cloid, symbol, side, px, sz, tif, reduce_only}` (no `asset`) consistent across Task 5 (definition) and Tasks 7, 8, 10 (callers).
- `CancelIntent` field set `{symbol, by_cloid, by_oid}` (no `asset`) consistent.
- `HlError::UnknownSymbol(Symbol)` used identically in Task 2 (definition), Task 3 (test), Task 6 (drop path), Task 8 (test).
- `order_intent_to_wire(intent: &OrderIntent, asset: u32)` signature consistent in Task 6.1 (definition) and Task 6.3 (call).
- `cancel_intent_to_wire(intent: &CancelIntent, asset: u32)` newly defined in Task 6.2 — actually, on reread, Task 6.4 doesn't call `cancel_intent_to_wire` directly; it constructs `CancelByCloidWire` inline. Both forms work, but consistency would favor using the helper. Decision: Task 6.4 stays inline because the inline form is cleaner with the loop's match arm; the helper from Task 6.2 is for any future reuse. Documented as a minor inconsistency to revisit if Gemini flags it.

**Edge case acknowledged:** `parse_exchange_response` / `parse_cancel_response` currently take `&[OrderIntent]` / `&[CancelIntent]` rather than `&[Cloid]`. The new code path in Task 6 has to clone `Vec<OrderIntent>` to feed it. The plan flags this as a follow-up refactor (deferred to PR-C2 or later) rather than fixing it inline, because changing those signatures touches both place_orders + cancel_orders + their existing tests. The clone cost is negligible (small N, infrequent path).

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-05-pr-c1-meta-cache-and-real-mode-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**