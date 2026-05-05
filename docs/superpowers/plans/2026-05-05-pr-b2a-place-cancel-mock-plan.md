# PR-B2a: place_orders / cancel_orders + mock backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete `RealHlClient::place_orders` and `cancel_orders` so they sign HL L1 actions with `Eip712AgentSigner`, POST to `/exchange`, and parse the response into `OrderResponse`. Add a `vault: Option<&Address>` parameter to `Signer::sign_l1`. Verify with mockito-mocked HL responses + activate the 2 vault-bearing cross-check vectors (10/10 pass).

**Architecture:** Add a `pack_action`-shaped `CancelByCloidAction` / `CancelByCloidWire` to the `eip712` module and a `dispatch_and_hash` arm for `cancelByCloid`. Add a workspace-wide `OrderIntent.asset: u32` and `CancelIntent.asset: u32` so `RealHlClient` can produce the wire shape directly without an async symbol→index lookup at every call. Replace the existing `RealHlClient::place_orders` / `cancel_orders` stubs with real implementations that call the new `post_exchange` helper and a pair of `parse_*_response` functions. mockito (1.7.2) is added as dev-dep to mock HL `/exchange` responses for at least 7 tests covering happy path, per-order error, top-level err, by_oid reject, and empty input.

**Tech Stack:** Rust 2021 (workspace MSRV 1.91), existing `alloy 2.0.4` + `rmp-serde 1.3.1` + `hex 0.4.3` + `secrecy 0.10`, new dev-dep `mockito 1.7.2`, existing `tokio` async + `serde_json` + `rust_decimal`.

---

## File Structure

| Path | Responsibility | Action |
|---|---|---|
| `executor/Cargo.toml` | `[workspace.dependencies]` add `mockito = "1.7.2"` | Modify |
| `executor/crates/executor-hl/Cargo.toml` | `[dev-dependencies]` add `mockito = { workspace = true }` | Modify |
| `executor/crates/executor-core/src/intent.rs` | Add `pub asset: u32` to `OrderIntent` and `CancelIntent` | Modify |
| `executor/crates/executor-hl/src/signer.rs` | Add `vault: Option<&Address>` parameter to `Signer::sign_l1` and update both `MockSigner` and `Eip712AgentSigner` impls. Adjust dispatch_and_hash to thread vault. Adjust the 3 existing MockSigner unit tests | Modify |
| `executor/crates/executor-hl/src/eip712.rs` | Append `CancelByCloidAction`, `CancelByCloidWire` structs. Append `cancelByCloid` arm to `dispatch_and_hash` (in signer.rs but it lives there). Append `order_intent_to_wire` helper | Modify |
| `executor/crates/executor-hl/src/hl_client.rs` | Replace stub `place_orders` / `cancel_orders` with real impls. Add private `post_exchange` helper (mirror of `post_info`). Add free functions `parse_exchange_response` / `parse_cancel_response`. Update existing call sites of `signer.sign_l1` to pass `None` | Modify |
| `executor/crates/executor-hl/tests/signing_cross_check.rs` | Remove the vault skip branch; pass `vault: Option<&Address>` through Signer trait so `dummy_with_vault_*` vectors run; assert 10/10 pass | Modify |
| `executor/crates/executor-hl/tests/place_cancel_mock.rs` | NEW. mockito-based integration tests for place_orders / cancel_orders | Create |
| `executor/crates/executor-hl/src/ws_state.rs:221` | Update internal test fixture `intent()` to set `asset: 0` (existing constructor) | Modify |
| `executor/crates/executor-hl/src/batch_sender.rs:240` | Update internal test fixture `order()` to set `asset: 0` | Modify |
| `executor/crates/executor-algo/src/market.rs:277` | Add `asset` field to `OrderIntent` constructor (use `0` placeholder pending PR-B2b) | Modify |
| `executor/crates/executor-algo/src/twap.rs:270, 299` | Same | Modify |
| `executor/crates/executor-algo/src/market_make.rs:386, 416` | Same | Modify |
| `executor/crates/executor-algo/src/passive_follow.rs` | Same (search for OrderIntent constructors) | Modify |
| `docs/HANDOFF-2026-05-04.md` | Append PR-B2a completion line | Modify |

**Why split this way:** `OrderIntent.asset` lives in `executor-core` because it's a domain field. Wire conversion (`order_intent_to_wire`) and EIP-712 (`CancelByCloidAction`) live in `eip712.rs` because they belong to the wire layer. `RealHlClient` orchestrates sign + POST + parse, but the response parsers are free functions so they can be unit tested without a `RealHlClient` instance. mockito tests live in `tests/place_cancel_mock.rs` (separate from `signing_cross_check.rs`) because they exercise a different concern (HTTP wire) and shouldn't share the cross-check fixture.

---

## Task 1: Branch + dependency wiring

**Files:**
- Modify: `executor/Cargo.toml`
- Modify: `executor/crates/executor-hl/Cargo.toml`

- [ ] **Step 1.1: Branch from develop**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git fetch origin
git checkout develop
git pull --rebase origin develop
git checkout -b feat/pr-b2a-place-cancel-mock
```

- [ ] **Step 1.2: Add mockito to workspace deps**

Edit `executor/Cargo.toml`. Find the `# Testing` block in `[workspace.dependencies]` (currently has `mockall`, `proptest`, `rstest`). Add `mockito = "1.7.2"` after `rstest = "0.24"`:

```toml
# Testing
mockall = "0.13"
proptest = "1"
rstest = "0.24"
mockito = "1.7.2"
```

- [ ] **Step 1.3: Wire mockito into executor-hl dev-deps**

Edit `executor/crates/executor-hl/Cargo.toml`. In the `[dev-dependencies]` block, add:

```toml
mockito = { workspace = true }
```

- [ ] **Step 1.4: Verify build still clean**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | tail -10`
Expected: success.

- [ ] **Step 1.5: Verify existing tests still pass**

Run: `cd executor && cargo test --workspace 2>&1 | grep "test result" | tail -10`
Expected: 133 tests pass (post-PR-B1 baseline).

- [ ] **Step 1.6: Commit**

```bash
git add executor/Cargo.toml executor/crates/executor-hl/Cargo.toml
git commit -m "build(executor-hl): add mockito 1.7.2 dev-dep for PR-B2a mock backend"
```

---

## Task 2: Add `asset: u32` to `OrderIntent` and `CancelIntent`

**Files:**
- Modify: `executor/crates/executor-core/src/intent.rs` (add `asset` field to both structs)
- Modify: 25 call sites across the workspace (each `OrderIntent { ... }` / `CancelIntent { ... }` literal)

- [ ] **Step 2.1: Confirm exact call site count**

Run: `cd /home/o9oem/workspace/crypto/diff-old-new && grep -rn "OrderIntent {\|CancelIntent {" executor/crates/ --include="*.rs"`
Expected: 25 total hits as confirmed by the spec §6.1 survey.

If the count differs, the call sites are documented in the output — fix all of them in this task. Do not skip any.

- [ ] **Step 2.2: Edit `intent.rs` to add `asset` field**

Edit `executor/crates/executor-core/src/intent.rs`. Replace the `OrderIntent` struct (around line 71-81) with:

```rust
/// One order to be sent (pre-batch). Algorithms enqueue these to the BatchSender.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub cloid: Cloid,
    pub symbol: Symbol,
    /// HL universe asset index (perp index, or 10000 + spot index). Required
    /// at the wire layer; resolved from `symbol` via the meta cache at the
    /// caller side (typically executor-server startup).
    pub asset: u32,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
}
```

Replace the `CancelIntent` struct (around line 84-90) with:

```rust
/// One cancel request (pre-batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelIntent {
    pub symbol: Symbol,
    /// HL universe asset index. Same source as `OrderIntent.asset`.
    pub asset: u32,
    /// Either oid (exchange-returned) or cloid (client-generated). cloid preferred.
    pub by_cloid: Option<Cloid>,
    pub by_oid: Option<OrderId>,
}
```

- [ ] **Step 2.3: Run cargo build to surface every broken call site**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | grep -E "error\[|missing field" | head -40`
Expected: ~25 `missing field 'asset'` errors. Note each `file:line` printed.

- [ ] **Step 2.4: Fix every `OrderIntent { ... }` and `CancelIntent { ... }` literal**

For each error from Step 2.3, add `asset: 0,` (or a more meaningful value if the surrounding test uses a specific symbol — e.g. `asset: 1` for ETH if symbol is `Symbol::new("ETH")`, but `0` is fine for tests that just need a value).

Use Edit tool, one file at a time. The 25 call sites split across:
- `executor/crates/executor-hl/src/ws_state.rs:222` (test fn `intent`)
- `executor/crates/executor-hl/src/hl_client.rs:538` (test fn `mock_records_orders_and_cancels`)
- `executor/crates/executor-hl/src/batch_sender.rs:240` (test fn `order`)
- `executor/crates/executor-algo/src/twap.rs:270, 299` (algorithm runtime)
- `executor/crates/executor-algo/src/market.rs:277` (algorithm runtime)
- `executor/crates/executor-algo/src/market_make.rs:386, 416` (algorithm runtime)
- `executor/crates/executor-algo/src/passive_follow.rs` (algorithm runtime; grep for line numbers)
- Additional test fixtures inside the same files

For algorithm runtime sites (twap.rs, market.rs, market_make.rs, passive_follow.rs), the algorithm doesn't yet know the asset index. Add `asset: 0,` for now and a `// TODO(PR-B2b): resolve via meta cache` comment. Document this systemically — do not hide it.

For `CancelIntent`, do the same with `asset: 0,`.

- [ ] **Step 2.5: Verify everything builds**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 2.6: Verify all 133 tests still pass**

Run: `cd executor && cargo test --workspace 2>&1 | grep "test result" | tail -10`
Expected: 133 tests pass (asset field is data-only, no logic depends on it yet).

- [ ] **Step 2.7: Run clippy**

Run: `cd executor && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 2.8: Commit**

```bash
git add executor/crates/executor-core/src/intent.rs \
        executor/crates/executor-hl/ \
        executor/crates/executor-algo/
git commit -m "feat(executor-core): add asset:u32 to OrderIntent and CancelIntent

Prerequisite for PR-B2a's RealHlClient::place_orders/cancel_orders
which need the HL universe asset index in the wire payload.

All algorithm-runtime callers receive asset:0 placeholder + TODO
comment pending PR-B2b's symbol→index resolution via meta cache.
Test-fixture callers receive asset:0.

133 baseline tests still pass (no logic depends on asset yet)."
```

---

## Task 3: Add `vault: Option<&Address>` to `Signer::sign_l1`

**Files:**
- Modify: `executor/crates/executor-hl/src/signer.rs`

- [ ] **Step 3.1: Update the trait signature**

Edit `executor/crates/executor-hl/src/signer.rs`. Find the `Signer` trait (around line 30-40). Replace `sign_l1` with:

```rust
    /// Sign an L1 action with a specific nonce.
    /// `vault` allows trading on behalf of a vault/subaccount; pass `None`
    /// for direct master/agent action.
    async fn sign_l1(
        &self,
        action: &Action,
        nonce: u64,
        vault: Option<&Address>,
    ) -> Result<Signature, HlError>;
```

- [ ] **Step 3.2: Update `MockSigner` impl signature**

Find `impl Signer for MockSigner` (around line 70-78). Replace `sign_l1` body with:

```rust
    async fn sign_l1(
        &self,
        _action: &Action,
        nonce: u64,
        _vault: Option<&Address>,
    ) -> Result<Signature, HlError> {
        Ok(Signature {
            r: format!("0x{:064x}", nonce),
            s: format!("0x{:064x}", nonce.wrapping_add(1)),
            v: 27,
        })
    }
```

- [ ] **Step 3.3: Update `Eip712AgentSigner` impl signature**

Find `impl Signer for Eip712AgentSigner` (around line 109-135). Replace `sign_l1` with:

```rust
    async fn sign_l1(
        &self,
        action: &Action,
        nonce: u64,
        vault: Option<&Address>,
    ) -> Result<Signature, HlError> {
        // Convert executor_core::types::Address (String wrapper) to alloy
        // 20-byte typed Address for action_hash. Parse failures surface as
        // ActionFormat (dynamic input data, not config).
        let vault_alloy = vault
            .map(|a| {
                a.as_str()
                    .parse::<AlloyAddress>()
                    .map_err(|e| HlError::ActionFormat(format!("vault address parse: {e}")))
            })
            .transpose()?;

        let hash = dispatch_and_hash(action, nonce, vault_alloy.as_ref())?;
        let agent = build_agent(hash, self.is_mainnet);
        let signing_hash = agent.eip712_signing_hash(&l1_domain());

        let raw_sig = self
            .inner
            .sign_hash_sync(&signing_hash)
            .map_err(|e| HlError::Signature(format!("sign_hash: {e}")))?;

        let v_byte: u8 = if raw_sig.v() { 28 } else { 27 };

        Ok(Signature {
            r: format!("0x{:064x}", raw_sig.r()),
            s: format!("0x{:064x}", raw_sig.s()),
            v: v_byte,
        })
    }
```

- [ ] **Step 3.4: Fix the 3 existing MockSigner unit tests**

Find `mod tests` at the bottom of `signer.rs` (around line 178-205). Update each `sign_l1` call site:

- Line ~192: `s.sign_l1(&json!({"x": 1}), 12345).await.unwrap();` → `s.sign_l1(&json!({"x": 1}), 12345, None).await.unwrap();`
- Line ~193: `s.sign_l1(&json!({"y": 2}), 12345).await.unwrap();` → `s.sign_l1(&json!({"y": 2}), 12345, None).await.unwrap();`
- Line ~201: `s.sign_l1(&json!({}), 1).await.unwrap();` → `s.sign_l1(&json!({}), 1, None).await.unwrap();`
- Line ~202: `s.sign_l1(&json!({}), 2).await.unwrap();` → `s.sign_l1(&json!({}), 2, None).await.unwrap();`

- [ ] **Step 3.5: Verify build (the cross-check test will still fail; expected at this stage)**

Run: `cd executor && cargo build -p executor-hl --all-targets 2>&1 | tail -10`
Expected: clean (the tests file doesn't compile yet — that's Task 4).

If `signing_cross_check.rs` has compile errors here, that's because it's still calling the 2-arg form. Don't fix here — Task 4 rewrites it. To temporarily compile, the cross_check test can be left broken until Task 4. If `cargo build` itself fails (not test build), examine the error — likely something else.

Actually: integration tests are part of `cargo build --all-targets`. If they fail to compile, this Step's build will fail. To unblock the rest of this task, run instead:

Run: `cd executor && cargo build -p executor-hl --lib 2>&1 | tail -5`
Expected: clean (lib only, skips tests).

- [ ] **Step 3.6: Run unit tests in signer.rs (lib tests, not integration)**

Run: `cd executor && cargo test -p executor-hl --lib signer:: 2>&1 | tail -10`
Expected: 3 MockSigner tests pass (mock_signer_address_is_stable, mock_signer_sign_is_deterministic_per_nonce, mock_signer_different_nonce_different_sig).

- [ ] **Step 3.7: Commit**

```bash
git add executor/crates/executor-hl/src/signer.rs
git commit -m "feat(executor-hl): add vault: Option<&Address> to Signer::sign_l1

Trait extension. MockSigner ignores vault (existing dummy preserved).
Eip712AgentSigner converts executor_core Address (String) to alloy
20-byte Address and threads it through dispatch_and_hash.

Existing 3 MockSigner unit tests updated with None argument.

Cross-check test in tests/signing_cross_check.rs is temporarily broken
at this commit — Task 4 rewrites it to pass vault and activate the
2 vault-bearing vectors (10/10 cross-check)."
```

---

## Task 4: Activate the 2 vault cross-check vectors (10/10 pass)

**Files:**
- Modify: `executor/crates/executor-hl/tests/signing_cross_check.rs`

- [ ] **Step 4.1: Update the cross-check test**

Edit `executor/crates/executor-hl/tests/signing_cross_check.rs`. The current test skips `vault_address.is_some()` cases. Now we pass vault through.

Find the section that constructs the address parameter (around the `for v in vectors()` loop). Replace the loop body with:

```rust
        // Parse vault if present (executor_core::types::Address is a String wrapper
        // so we just wrap the hex string from the fixture).
        let vault: Option<executor_core::types::Address> = v
            .vault_address
            .as_ref()
            .map(|s| executor_core::types::Address::new(s));

        let signer = Eip712AgentSigner::from_secret(
            SecretString::new(TEST_PK.into()),
            v.is_mainnet,
        )
        .unwrap();

        // Address sanity check
        assert_eq!(
            signer.address().as_str().to_lowercase(),
            v.expected_address,
            "address mismatch for {}",
            v.name
        );

        let sig = match signer.sign_l1(&v.action, v.nonce, vault.as_ref()).await {
            Ok(s) => s,
            Err(e) => {
                failed.push(format!("{}: sign_l1 errored: {e}", v.name));
                continue;
            }
        };

        // ... existing r/s/v compare with norm_hex32 stays as-is ...
```

Remove the early-skip branch:

```rust
        // DELETE THIS BLOCK:
        // if v.vault_address.is_some() {
        //     skipped.push(v.name.clone());
        //     eprintln!("SKIP (vault not yet supported): {}", v.name);
        //     continue;
        // }
```

Update the summary print:

```rust
    eprintln!("\n=== cross-check summary ===");
    eprintln!("vectors checked: 10 (all)");
    if !failed.is_empty() {
        panic!("\ncross-check failures:\n{}\n", failed.join("\n\n"));
    }
```

Remove the `skipped` Vec and its push site (since nothing is skipped).

- [ ] **Step 4.2: Run the cross-check**

Run: `cd executor && cargo test -p executor-hl --test signing_cross_check -- --nocapture 2>&1 | tail -40`
Expected: 3 tests in this file pass; the `cross_check_all_known_vectors` summary says "vectors checked: 10 (all)" and no failures.

If `dummy_with_vault_*` fails: the `dispatch_and_hash` correctly threads the alloy `Address`, but verify the byte serialization. The vault prefix in `action_hash` is `0x01 || address_bytes` (20 bytes raw). If those bytes match the python-sdk format, the hash matches. If failing:
1. Add temp `eprintln!("vault bytes: {:?}", vault_alloy.as_ref().map(|a| hex::encode(a.as_slice())));` to Eip712AgentSigner::sign_l1.
2. Compare to python: `python3 -c "from hyperliquid.utils.signing import action_hash; import binascii; h = action_hash({'type':'dummy','num':100000000000}, '0x1719884eb866cb12b2287399b15f7db5e7d775ea', 0, None); print(binascii.hexlify(h).decode())"`
3. If hashes differ, it's a vault-bytes encoding bug.

- [ ] **Step 4.3: Run the whole executor-hl crate**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: existing tests + 3 cross-check tests = 137 in executor-hl alone (was 134 with PR-B1's 8/10; now 10/10).

Workspace: `cd executor && cargo test --workspace 2>&1 | grep "test result" | tail -5`
Expected: 135 total (was 133, +2 from vault enable in cross_check_all_known_vectors).

Wait — the cross_check_all_known_vectors is one test that internally loops over vectors. Total test count in workspace doesn't change; only the assertions inside the loop go from 8 to 10. Verify by counting the test names:

```bash
cd executor && cargo test -p executor-hl --test signing_cross_check -- --list 2>&1 | grep ":"
```
Expected: 3 test names (`fixture_loads_with_10_vectors`, `signer_address_matches_test_pk`, `cross_check_all_known_vectors`).

- [ ] **Step 4.4: Clippy**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4.5: Commit**

```bash
git add executor/crates/executor-hl/tests/signing_cross_check.rs
git commit -m "test(executor-hl): activate 2 vault cross-check vectors (10/10 pass)

Removes the vault skip branch added in PR-B1. Now passes vault address
through Signer::sign_l1 and asserts dummy_with_vault_{mainnet,testnet}
match the HL python-sdk byte-identically.

10 of 10 known vectors verified."
```

---

## Task 5: Add `CancelByCloidAction` and `cancelByCloid` dispatch

**Files:**
- Modify: `executor/crates/executor-hl/src/eip712.rs` (append CancelByCloid types)
- Modify: `executor/crates/executor-hl/src/signer.rs` (extend dispatch_and_hash)

- [ ] **Step 5.1: Append cancel wire structs to eip712.rs**

Edit `executor/crates/executor-hl/src/eip712.rs`. Find the `// === action types (dict-order matched to HL python-sdk) ===` block. After `ScheduleCancelAction` definition, append:

```rust
/// `{"type": "cancelByCloid", "cancels": [{asset, cloid}, ...]}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub cancels: Vec<CancelByCloidWire>,
}

/// One cancel wire item. Field order: asset (full word, NOT `a`), cloid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelByCloidWire {
    pub asset: u32,
    pub cloid: String,
}
```

- [ ] **Step 5.2: Add `cancelByCloid` arm to dispatch_and_hash in signer.rs**

Edit `executor/crates/executor-hl/src/signer.rs`. Find the existing `match kind` in `dispatch_and_hash` (around line 152-174). Add a new arm before the `other =>` fallback. Also extend the imports at the top of signer.rs:

Add to the `use crate::eip712::{...}` import:
```rust
use crate::eip712::{action_hash, build_agent, l1_domain};
use crate::eip712::{CancelByCloidAction, DummyAction, OrderAction, ScheduleCancelAction};
```

Add to the match:
```rust
        "cancelByCloid" => {
            let typed = CancelByCloidAction::deserialize(action)
                .map_err(|e| HlError::ActionFormat(format!("cancelByCloid decode: {e}")))?;
            action_hash(&typed, nonce, vault, None)
                .map_err(|e| HlError::ActionFormat(format!("cancelByCloid msgpack: {e}")))
        }
```

- [ ] **Step 5.3: Add a unit test for cancelByCloid round-trip in eip712::tests**

Edit `executor/crates/executor-hl/src/eip712.rs`. Inside the existing `mod tests` block, append:

```rust
    /// `pack_action(&CancelByCloidAction{...})` must serialize as a msgpack map
    /// with `type`/`cancels` keys and per-cancel `asset`/`cloid` keys (named map form).
    /// Sanity check only — full byte match isn't required because the cross-check
    /// fixture covers signing end-to-end via dispatch_and_hash.
    #[test]
    fn cancel_by_cloid_action_msgpack_starts_with_map_marker() {
        let action = CancelByCloidAction {
            action_type: "cancelByCloid".into(),
            cancels: vec![CancelByCloidWire {
                asset: 1,
                cloid: "0x00000000000000000000000000000001".into(),
            }],
        };
        let bytes = pack_action(&action).unwrap();
        // 0x82 = fix-map(2) — top-level has "type" + "cancels"
        assert_eq!(bytes[0], 0x82, "expected fix-map(2), got 0x{:02x}", bytes[0]);
    }
```

- [ ] **Step 5.4: Run the new test**

Run: `cd executor && cargo test -p executor-hl eip712::tests::cancel_by_cloid -- --nocapture`
Expected: 1 pass.

- [ ] **Step 5.5: Run all executor-hl tests**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: previous count + 1 new test, still all green.

- [ ] **Step 5.6: Clippy**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 5.7: Commit**

```bash
git add executor/crates/executor-hl/src/eip712.rs executor/crates/executor-hl/src/signer.rs
git commit -m "feat(executor-hl): CancelByCloidAction wire type + dispatch arm

CancelByCloidAction { type, cancels: [{asset, cloid}] }. asset uses
full word (not 'a' shorthand) per HL exchange spec. dispatch_and_hash
gains a cancelByCloid arm so Eip712AgentSigner can sign cancel actions.

Sanity unit test asserts pack_action starts with fix-map(2) marker."
```

---

## Task 6: `RealHlClient::place_orders` real implementation

**Files:**
- Modify: `executor/crates/executor-hl/src/hl_client.rs`

- [ ] **Step 6.1: Add `order_intent_to_wire` helper to eip712.rs**

Edit `executor/crates/executor-hl/src/eip712.rs`. After the cancel structs from Task 5, append:

```rust
use executor_core::intent::OrderIntent;
use executor_core::types::{Side, Tif};

/// Convert an `OrderIntent` (executor-core domain type) into the HL wire
/// shape `OrderWire`.
///
/// The caller passes `OrderIntent.asset` directly (set at intent construction
/// time, currently 0 for algorithm-runtime callers — to be resolved via
/// meta cache in PR-B2b).
pub fn order_intent_to_wire(intent: &OrderIntent) -> OrderWire {
    OrderWire {
        a: intent.asset,
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

Verify import `use executor_core::intent::OrderIntent;` doesn't conflict with anything else in eip712.rs (it's a fresh import in this file).

- [ ] **Step 6.2: Add `post_exchange` helper to RealHlClient**

Edit `executor/crates/executor-hl/src/hl_client.rs`. Find the existing `impl RealHlClient { ... }` block that contains `post_info` (around line 387-405). Add `post_exchange` as a sibling:

```rust
impl RealHlClient {
    // ... existing post_info ...

    /// POST a JSON body to the /exchange endpoint and return the response body.
    /// Mirrors `post_info` but targets `config.exchange_url`.
    async fn post_exchange(&self, body: &serde_json::Value) -> Result<String, HlError> {
        let resp = self
            .http
            .post(&self.config.exchange_url)
            .json(body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(HlError::Network(format!("HTTP {status}: {text}")));
        }
        Ok(text)
    }
}
```

- [ ] **Step 6.3: Add the free `parse_exchange_response` function**

In the same `hl_client.rs` file, near the other free functions (above the `impl HlClient for RealHlClient` block), add:

```rust
/// Parse HL `/exchange` response for an order action into per-order `OrderResponse`.
///
/// Recognized status shapes per element of `response.data.statuses`:
/// - `{"resting": {"oid": <u64>}}` -> status="resting"
/// - `{"filled": {"oid": <u64>, "totalSz": "...", "avgPx": "..."}}` -> status="filled"
/// - `{"error": "<msg>"}` -> status="error"
/// - top-level `{"status":"err", "response": "<msg>"}` -> Err(Exchange)
fn parse_exchange_response(
    text: &str,
    orders: &[OrderIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse exchange json: {e}")))?;

    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("(no msg)");
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
            statuses.len(),
            orders.len()
        )));
    }

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(orders.iter()) {
        let cloid = intent.cloid;
        if let Some(resting) = status.get("resting") {
            let oid = resting
                .get("oid")
                .and_then(|o| o.as_u64())
                .ok_or_else(|| HlError::InvalidResponse("resting.oid missing".into()))?;
            out.push(OrderResponse {
                cloid,
                oid: Some(OrderId(oid)),
                status: "resting".into(),
                error: None,
            });
        } else if let Some(filled) = status.get("filled") {
            let oid = filled
                .get("oid")
                .and_then(|o| o.as_u64())
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

- [ ] **Step 6.4: Replace `RealHlClient::place_orders` body**

Find the existing `async fn place_orders(...)` in `impl HlClient for RealHlClient` (around line 487-510). Replace its body with:

```rust
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }

        let weight = 1 + (orders.len() as u32 / 40);
        let _wait = self.rate_limiter.acquire(weight).await;

        let order_wires: Vec<crate::eip712::OrderWire> =
            orders.iter().map(crate::eip712::order_intent_to_wire).collect();

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

        // PR-B2a: vault always None. PR-B2b will plumb vault through `place_orders`.
        let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

        let body = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": sig,
            "vaultAddress": serde_json::Value::Null,
        });

        let resp_text = self.post_exchange(&body).await?;
        parse_exchange_response(&resp_text, orders)
    }
```

- [ ] **Step 6.5: Verify build**

Run: `cd executor && cargo build -p executor-hl --all-targets 2>&1 | tail -10`
Expected: clean.

- [ ] **Step 6.6: Verify existing tests still pass (mock path unchanged)**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: all pass (no behavior change for `MockHlClient`; only `RealHlClient::place_orders` body changed and there are no existing tests against `RealHlClient::place_orders` in the mock — those land in Task 8).

- [ ] **Step 6.7: Clippy**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 6.8: Commit**

```bash
git add executor/crates/executor-hl/src/hl_client.rs executor/crates/executor-hl/src/eip712.rs
git commit -m "feat(executor-hl): RealHlClient::place_orders real implementation

- order_intent_to_wire(): OrderIntent -> eip712::OrderWire conversion
  using rust_decimal Display (canonical form, matches HL python-sdk
  float_to_wire output for plain decimals).
- post_exchange(): POST /exchange helper mirroring post_info.
- parse_exchange_response(): handles resting/filled/error status shapes
  + top-level err -> HlError::Exchange.
- place_orders(): builds OrderAction, signs via Eip712AgentSigner
  (vault=None for PR-B2a), posts, parses.

Mock backend tests land in Task 8."
```

---

## Task 7: `RealHlClient::cancel_orders` real implementation

**Files:**
- Modify: `executor/crates/executor-hl/src/hl_client.rs`

- [ ] **Step 7.1: Add `parse_cancel_response` free function**

Edit `executor/crates/executor-hl/src/hl_client.rs`. Below `parse_exchange_response`, add:

```rust
/// Parse HL `/exchange` response for a cancelByCloid action.
///
/// HL returns cancel success as the bare string `"success"` (NOT an object,
/// unlike order responses). Per-cancel error is `{"error": "<msg>"}`.
fn parse_cancel_response(
    text: &str,
    cancels: &[CancelIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse cancel json: {e}")))?;

    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("(no msg)");
        return Err(HlError::Exchange {
            code: Some("top_level_err".into()),
            message: msg.into(),
        });
    }

    let statuses = v
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| HlError::InvalidResponse("cancel statuses missing".into()))?;

    if statuses.len() != cancels.len() {
        return Err(HlError::InvalidResponse(format!(
            "cancel statuses len {} != cancels len {}",
            statuses.len(),
            cancels.len()
        )));
    }

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(cancels.iter()) {
        let cloid = intent.by_cloid.unwrap_or_default();
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

- [ ] **Step 7.2: Replace `RealHlClient::cancel_orders` body**

Find the existing `async fn cancel_orders(...)` (around line 516-525). Replace its body with:

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

        // by_cloid only. by_oid is explicitly rejected (PR-B2a scope).
        let cancel_wires: Result<Vec<crate::eip712::CancelByCloidWire>, HlError> = cancels
            .iter()
            .map(|c| {
                if c.by_oid.is_some() {
                    return Err(HlError::ActionFormat(
                        "by_oid cancel not supported in PR-B2a; use by_cloid".into(),
                    ));
                }
                let cloid = c.by_cloid.ok_or_else(|| {
                    HlError::ActionFormat(
                        "CancelIntent missing both by_cloid and by_oid".into(),
                    )
                })?;
                Ok(crate::eip712::CancelByCloidWire {
                    asset: c.asset,
                    cloid: format!("{}", cloid),
                })
            })
            .collect();
        let cancel_wires = cancel_wires?;

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
        parse_cancel_response(&resp_text, cancels)
    }
```

- [ ] **Step 7.3: Build + tests**

Run:
```bash
cd executor
cargo build -p executor-hl --all-targets 2>&1 | tail -5
cargo test -p executor-hl 2>&1 | grep "test result" | tail -10
cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -5
```
Expected: build clean, tests all pass (mock path unchanged), clippy clean.

- [ ] **Step 7.4: Commit**

```bash
git add executor/crates/executor-hl/src/hl_client.rs
git commit -m "feat(executor-hl): RealHlClient::cancel_orders real implementation

- parse_cancel_response(): handles 'success' string + per-cancel error
  shapes + top-level err -> HlError::Exchange.
- cancel_orders(): builds CancelByCloidAction, signs, posts, parses.
- by_oid intents explicitly rejected with ActionFormat error per spec
  (PR-B2a scope: cancelByCloid only)."
```

---

## Task 8: mockito-based integration tests

**Files:**
- Create: `executor/crates/executor-hl/tests/place_cancel_mock.rs`

- [ ] **Step 8.1: Create the test file with all 7 tests**

Create `executor/crates/executor-hl/tests/place_cancel_mock.rs`:

```rust
//! Mock-backend integration tests for RealHlClient::place_orders/cancel_orders.
//!
//! Uses mockito to mock HL /exchange responses. No real network, no PK
//! beyond the well-known test PK from PR-B1's signing fixture. Real
//! testnet smoke is in PR-B2b.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::cloid::Cloid;
use executor_core::intent::{CancelIntent, OrderIntent};
use executor_core::symbol::Symbol;
use executor_core::types::{OrderId, Side, Tif};
use executor_hl::errors::HlError;
use executor_hl::hl_client::{HlClient, HlConfig, RealHlClient};
use executor_hl::signer::Eip712AgentSigner;
use rust_decimal_macros::dec;
use secrecy::SecretString;
use std::sync::Arc;

const TEST_PK: &str =
    "0x0123456789012345678901234567890123456789012345678901234567890123";

fn make_client(server_url: &str) -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap(),
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
        asset: 1,
        side: Side::Long,
        px: dec!(2000),
        sz: dec!(0.001),
        tif: Tif::Alo,
        reduce_only: false,
    }
}

fn make_cancel_intent(cloid: Cloid) -> CancelIntent {
    CancelIntent {
        symbol: Symbol::new("ETH"),
        asset: 1,
        by_cloid: Some(cloid),
        by_oid: None,
    }
}

#[tokio::test]
async fn place_orders_resting_response_parses_to_oid() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/exchange")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":12345}}]}}}"#,
        )
        .create_async()
        .await;

    let client = make_client(&server.url());
    let resp = client.place_orders(&[make_order_intent()]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "resting");
    assert_eq!(resp[0].oid, Some(OrderId(12345)));
    assert!(resp[0].error.is_none());
}

#[tokio::test]
async fn place_orders_filled_response_parses_to_oid_and_filled_status() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/exchange")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"oid":67890,"totalSz":"0.001","avgPx":"2000.0"}}]}}}"#,
        )
        .create_async()
        .await;

    let client = make_client(&server.url());
    let resp = client.place_orders(&[make_order_intent()]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "filled");
    assert_eq!(resp[0].oid, Some(OrderId(67890)));
    assert!(resp[0].error.is_none());
}

#[tokio::test]
async fn place_orders_per_order_error_keeps_cloid_and_attaches_error() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/exchange")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"MinTradeNtl"}]}}}"#,
        )
        .create_async()
        .await;

    let client = make_client(&server.url());
    let intent = make_order_intent();
    let cloid = intent.cloid;
    let resp = client.place_orders(&[intent]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "error");
    assert_eq!(resp[0].cloid, cloid);
    assert!(resp[0].error.as_deref() == Some("MinTradeNtl"));
}

#[tokio::test]
async fn place_orders_top_level_err_returns_hl_error_exchange() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/exchange")
        .with_status(200)
        .with_body(r#"{"status":"err","response":"Insufficient margin"}"#)
        .create_async()
        .await;

    let client = make_client(&server.url());
    let err = client
        .place_orders(&[make_order_intent()])
        .await
        .unwrap_err();
    match err {
        HlError::Exchange { code, message } => {
            assert_eq!(code.as_deref(), Some("top_level_err"));
            assert!(
                message.contains("Insufficient margin"),
                "message was: {message}"
            );
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
async fn cancel_orders_success_string_response() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/exchange")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}"#,
        )
        .create_async()
        .await;

    let client = make_client(&server.url());
    let cloid = Cloid::new();
    let resp = client
        .cancel_orders(&[make_cancel_intent(cloid)])
        .await
        .unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "cancelled");
    assert_eq!(resp[0].cloid, cloid);
    assert!(resp[0].error.is_none());
}

#[tokio::test]
async fn cancel_orders_by_oid_returns_action_format_error() {
    let server = mockito::Server::new_async().await;
    // No mock needed — the error fires before any HTTP call.
    let client = make_client(&server.url());
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        asset: 1,
        by_cloid: None,
        by_oid: Some(OrderId(99999)),
    };
    let err = client.cancel_orders(&[cancel]).await.unwrap_err();
    match err {
        HlError::ActionFormat(msg) => {
            assert!(
                msg.contains("by_oid cancel not supported"),
                "msg was: {msg}"
            );
        }
        other => panic!("expected ActionFormat err, got {other:?}"),
    }
}
```

- [ ] **Step 8.2: Run the new tests**

Run: `cd executor && cargo test -p executor-hl --test place_cancel_mock -- --nocapture 2>&1 | tail -30`
Expected: 7 tests pass. If a test fails:
- network timeout / connection refused → mockito server not bound; check `server.url()` is being used
- `Insufficient margin` not found → response body string mismatch
- `unknown status shape` → the JSON pointer `/response/data/statuses` failed to find data; verify the mock body matches the spec's response shape

- [ ] **Step 8.3: Run the whole executor-hl crate**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep "test result" | tail -10`
Expected: previous total + 7 new tests, all pass.

- [ ] **Step 8.4: Workspace + clippy + fmt**

Run:
```bash
cd executor
cargo test --workspace 2>&1 | grep "test result" | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --all -- --check 2>&1 | tail -5
```
Expected: workspace 142 (was 133 + 7 new + 2 vault activated cross-check assertions inside one test = 7 new test names overall), clippy clean, fmt clean.

If `fmt --check` fails, run `cargo fmt --all` and stage the changes.

- [ ] **Step 8.5: Commit**

```bash
git add executor/crates/executor-hl/tests/place_cancel_mock.rs
# Plus any cargo fmt changes
git add -u
git commit -m "test(executor-hl): mockito integration tests for place/cancel

7 tests covering: resting/filled/error status shapes, top-level err,
empty input, cancel success ('success' string), by_oid reject (no
HTTP call expected). Asserts OrderResponse shape including cloid
preservation through error paths."
```

---

## Task 9: HANDOFF + final smoke

**Files:**
- Modify: `docs/HANDOFF-2026-05-04.md`

- [ ] **Step 9.1: Update HANDOFF doc**

Edit `docs/HANDOFF-2026-05-04.md`. Find the existing "PR-B1 完了" block (added at end of Step B in PR-B1's Task 6). Append a new line:

```
- 2026-05-05 PR-B2a 完了: place_orders/cancel_orders 実装 + mock 7 tests + cross-check 10/10 (vault 2 件 enable). Signer trait に vault: Option<&Address> 追加. cancelByCloid のみ実装 (by_oid は ActionFormat reject). r/s padding の実 HL 検証は PR-B2b.
```

- [ ] **Step 9.2: Run local CI smoke**

Run: `bash scripts/check_ci_local.sh 2>&1 | tail -15`
Expected: green ("All CI checks passed locally").

- [ ] **Step 9.3: Commit HANDOFF**

```bash
git add docs/HANDOFF-2026-05-04.md
git commit -m "docs: HANDOFF — PR-B2a (place/cancel + mock + 10/10 cross-check) merged"
```

---

## Task 10: Gemini deep review + open PR

**Files:**
- (none — review feedback may produce additional commits)

- [ ] **Step 10.1: Generate code-only diff**

Run:
```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git diff develop...HEAD -- \
  executor/crates/executor-hl/src/ \
  executor/crates/executor-hl/tests/place_cancel_mock.rs \
  executor/crates/executor-hl/tests/signing_cross_check.rs \
  executor/crates/executor-hl/Cargo.toml \
  executor/crates/executor-core/src/intent.rs \
  executor/crates/executor-algo/src/ \
  executor/Cargo.toml \
  > /tmp/pr-b2a-diff.patch
wc -l /tmp/pr-b2a-diff.patch
wc -c /tmp/pr-b2a-diff.patch | awk '{printf "%.1f KB\n", $1/1024}'
```

- [ ] **Step 10.2: Run Gemini deep review**

```bash
{
  echo "PR-B2a: RealHlClient::place_orders/cancel_orders 本実装 + mock test + cross-check 10/10."
  echo "Spec: docs/superpowers/specs/2026-05-05-pr-b2a-place-cancel-with-mock-design.md"
  echo "Plan: docs/superpowers/plans/2026-05-05-pr-b2a-place-cancel-mock-plan.md"
  echo
  echo "## 達成"
  echo "- Signer::sign_l1 に vault: Option<&Address> 追加."
  echo "- 既存 8/10 cross-check vector + vault 2 件 = 10/10 byte-identical."
  echo "- RealHlClient::place_orders 本実装 (eip712 OrderAction → sign → POST → parse)."
  echo "- RealHlClient::cancel_orders 本実装 (cancelByCloid のみ. by_oid は ActionFormat reject)."
  echo "- mockito 1.7.2 dev-dep 追加, 7 つの mock backend test."
  echo "- OrderIntent / CancelIntent に asset: u32 field 追加 (executor-algo の 25 caller 修正)."
  echo
  echo "## 観点"
  echo "1. 暗号学的安全性: Signer trait 拡張で漏れなく vault 渡しているか."
  echo "2. action_hash の vault prefix: 0x01 || 20bytes が python-sdk と一致するか."
  echo "3. response parser: filled/resting/error 各 shape を漏れなく扱っているか. 統計的に期待しない shape (例: filled object 内に oid なし) で誤動作しないか."
  echo "4. cancelByCloid の wire format: 'asset' フィールド名が正しいか (order の 'a' と異なる)."
  echo "5. OrderIntent.asset の placeholder=0 戦略: 各 algorithm caller が 0 を入れる現状, 本番運用で何が起きるか."
  echo "6. mockito test で signature 部分を assert しない判断: nonce time-based のため req body 完全一致できないが PartialJson でも action 部分は assert すべきか."
  echo "7. r/s padding: PR-B2a では mock の body assert しないので問題化しないが, PR-B2b で実 HL に弾かれるリスク評価."
  echo "8. by_oid reject の policy: emergency_stop で oid ベースの cancel が将来必要になる場合の拡張パス."
  echo
  echo "## 期待するレビュー"
  echo "- MUST-FIX: cryptographic / security / logic 問題."
  echo "- SHOULD-FIX: PR-B2b までに直すべき設計問題."
  echo "- SUGGESTION: PR-B2b 以降検討."
  echo "- 各指摘に file:line + 理由."
  echo
  echo "## Diff (~$(wc -l < /tmp/pr-b2a-diff.patch) lines, $(wc -c < /tmp/pr-b2a-diff.patch | awk '{printf \"%.1f\", $1/1024}') KB)"
  echo
  cat /tmp/pr-b2a-diff.patch
} | ~/.claude/hooks/gemini-review.sh deep --timeout 240 2>&1 | tee /tmp/pr-b2a-gemini-review.md | tail -150
```

- [ ] **Step 10.3: Address review comments**

For each MUST-FIX:
1. Make the change.
2. Re-run `cd executor && cargo test -p executor-hl` to ensure tests still pass.
3. Re-run `cargo clippy -p executor-hl --all-targets -- -D warnings`.
4. Commit each fix as its own commit (`fix(executor-hl): <comment summary>`).

For SHOULD-FIX/SUGGESTION items, decide per-item: apply if quick + low risk, defer to PR-B2b if larger.

- [ ] **Step 10.4: Push branch and open PR with `--base develop`**

```bash
git push -u origin feat/pr-b2a-place-cancel-mock
gh pr create --base develop --title "feat(executor-hl): PR-B2a — place_orders/cancel_orders + mock backend (10/10 cross-check)" --body "$(cat <<'EOF'
## Summary

Stage B step 2 (PR-B2a) of the C-1 段階的検証 spec. Real `/exchange` POST happens here for the first time, but verification is mock-only — actual testnet smoke lands in PR-B2b.

- `Signer::sign_l1` gains `vault: Option<&Address>`. `MockSigner` ignores it; `Eip712AgentSigner` parses to alloy 20-byte Address and threads through `dispatch_and_hash` → `action_hash`.
- 2 vault-bearing cross-check vectors (`dummy_with_vault_{mainnet,testnet}`) now activated → **10/10 byte-identical** to HL python-sdk.
- `eip712`: new `CancelByCloidAction` / `CancelByCloidWire` structs (note: cancel uses `asset` full word, NOT `a` shorthand). New `order_intent_to_wire` helper.
- `RealHlClient`: real `place_orders` / `cancel_orders` implementations. New private `post_exchange` helper. New free functions `parse_exchange_response` (resting/filled/error/top-err) and `parse_cancel_response` (`"success"` string + per-cancel error).
- `OrderIntent` / `CancelIntent` get `asset: u32` field (required for wire layer). 25 algorithm-runtime callers updated with `asset: 0` placeholder + `// TODO(PR-B2b)` comment pending meta-cache resolution.
- 7 mockito tests cover: resting/filled/error/top-level-err on place, success/by-oid-reject on cancel, plus empty-input.

## Test plan

- [x] `cd executor && cargo test --workspace` — 142 tests pass (was 133)
- [x] `cd executor && cargo clippy --workspace --all-targets -- -D warnings` — clean
- [x] `cd executor && cargo fmt --all -- --check` — clean
- [x] `bash scripts/check_ci_local.sh` — green
- [x] Cross-check: 10/10 vectors r/s/v match python-sdk byte-identically

## Notes

- `cancelByCloid` only. `CancelIntent.by_oid: Some(_)` returns `HlError::ActionFormat` immediately (no HTTP call). emergency_stop / by-oid path arrives in a later PR.
- mockito tests do NOT assert request body in detail (nonce is time-based, signature varies). Coverage is on response parsing.
- `vault: None` is hardcoded in `place_orders` / `cancel_orders` for PR-B2a. PR-B2b will plumb vault through these methods if subaccount support is needed.
- `r/s` are still always 64-hex padded (no leading-zero strip). Real HL acceptance verified in PR-B2b testnet smoke.

## Deferred (tracked for PR-B2b / later)

- Plumb `vault: Option<&Address>` parameter through `place_orders` / `cancel_orders` themselves (currently only on Signer trait).
- Resolve `OrderIntent.asset` via `fetch_meta` cache at executor-server startup; remove `asset: 0` placeholders from algo crate.
- testnet smoke (real `/exchange` POST) and r/s padding acceptance check.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Print PR URL.

- [ ] **Step 10.5: Watch CI and merge if green**

```bash
gh pr checks <PR_NUMBER> --watch --interval 15
# When both rust + python pass:
gh pr merge <PR_NUMBER> --squash --delete-branch
```

- [ ] **Step 10.6: Sync develop locally**

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
| §3.1 Eip712AgentSigner reuse | Task 3 (sign_l1 vault arg threaded), Task 6 (place_orders calls signer) |
| §3.2 Caller-impact survey (no external) | Task 2 (asset field — internal-only via grep result of 25), Task 3 (3 MockSigner test fixups) |
| §3.3 wire format (order/cancel/response) | Task 5 (CancelByCloid types), Task 6 (place + parse), Task 7 (cancel + parse) |
| §3.4 asset index resolution | Task 2 (asset field added) — dynamic resolution deferred to PR-B2b per spec |
| §3.5 cancel = cancelByCloid only, by_oid reject | Task 7 (Step 7.2 cancel_orders body) + Task 8 (test `cancel_orders_by_oid_returns_action_format_error`) |
| §3.6 r/s padding (PR-B2b) | Plan Task 8 mockito assertions don't check r/s padding; deferred per spec |
| §4 mockito 1.7.2 + alloy/rmp-serde/hex unchanged | Task 1 |
| §5.1 file structure | Tasks 1-9 each touch the listed files |
| §5.2 trait extension code | Task 3 (verbatim) |
| §5.3 cancel wire structs | Task 5 (verbatim) |
| §5.4 order_intent_to_wire | Task 6.1 (verbatim) |
| §5.5 place_orders impl | Task 6.4 (verbatim) |
| §5.6 cancel_orders impl | Task 7.2 (verbatim) |
| §5.7 mock test code | Task 8 (verbatim) |
| §5.8 OrderIntent.asset field check | Task 2 (Step 2.1 grep, Step 2.2 add field, Steps 2.3-2.4 fix all 25 callers) |
| §5.9 acceptance criteria | Task 10.5 (CI green covers all bullets) |
| §6.1 asset field migration | Task 2 (full process documented) |
| §6.2 cancel "success" string | Task 7.1 (parse_cancel_response handles it) |
| §6.3 mockito URL trailing slash | `make_client()` in Task 8.1 uses `format!("{server_url}/exchange")` (no trailing slash on server.url(), confirmed by mockito docs) |
| §6.4 nonce time-based, no body assert | Task 8 mock tests don't `match_body` |
| §6.5 vault parse → ActionFormat | Task 3 (Step 3.3 Eip712AgentSigner sign_l1 body) |
| §6.6 r/s padding deferred | (Spec / plan both note this; no test assertion) |

No spec gaps.

**Placeholder scan:** None of the patterns "TBD"/"TODO"/"implement later"/"similar to Task N" appear as placeholders in plan body. The `// TODO(PR-B2b): resolve via meta cache` comment in algo crates is intentional documentation of deferred work, not a plan placeholder.

**Type consistency:**
- `OrderIntent.asset: u32` defined in Task 2 used in Task 6 (`order_intent_to_wire`) and Task 8 (test fixtures).
- `CancelIntent.asset: u32` defined in Task 2 used in Task 7 (`cancel_orders` body) and Task 8 (test fixtures + by_oid reject test).
- `Signer::sign_l1(action, nonce, vault)` signature consistent across Task 3 (trait + 2 impls + 3 test updates), Task 4 (cross-check call), Task 6 (place_orders call), Task 7 (cancel_orders call).
- `CancelByCloidAction` / `CancelByCloidWire` defined in Task 5 used in Task 7.
- `parse_exchange_response` / `parse_cancel_response` are free functions defined in Tasks 6/7 with consistent signatures `(text: &str, intents: &[X]) -> Result<Vec<OrderResponse>, HlError>`.
- `HlError::ActionFormat` introduced in PR-B1 used consistently for vault parse failures (Task 3), by_oid rejection (Task 7), serialize errors (Tasks 6/7).
- `executor_core::types::Address` (String wrapper) vs `alloy::primitives::Address` (20-byte) clearly distinguished in Task 3 with explicit conversion + ActionFormat error mapping.

**Edge case acknowledged:** `OrderIntent.asset` migration touches 25 call sites. Task 2 makes this explicit with a Step 2.1 grep that surfaces every site, Step 2.2 adds the field, Step 2.3 surfaces compile errors with specific file:line, Step 2.4 fixes them with placeholder `asset: 0` + comment. This is the largest single-task scope in the plan; the alternative of `Option<u32>` was rejected because it pushes the asset-resolution responsibility to runtime where it would silently fail.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-05-pr-b2a-place-cancel-mock-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**