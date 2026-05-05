# PR-A: HL Mainnet Read-Only Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete `RealHlClient::fetch_account_state` and `fetch_book_snapshot` so they fully parse Hyperliquid `/info` responses into `AccountStateSnapshot` / `OrderBook` structs, with sanitized JSON fixtures from real mainnet data and an opt-in `live` integration test.

**Architecture:** Extend the existing `executor-hl` crate with strongly-typed wire structs that mirror HL's JSON schema (using `#[serde(with = "rust_decimal::serde::str")]` to deserialize string-encoded numbers into `Decimal`), then map them into the existing `executor-core::state::{Position, OrderBook}` types. Multi-dex support is added via an optional `dex` parameter on the trait method signatures. Live tests are gated behind a `live` feature flag (default off in CI).

**Tech Stack:** Rust 2021, `executor-hl` crate, `reqwest` 0.13, `serde` + `serde_json`, `rust_decimal` (with `serde-with-str` already enabled), `chrono`, `tokio` async, `rstest` for parameterized tests, `mockall` (existing).

---

## File Structure

| Path | Responsibility | Action |
|---|---|---|
| `executor/crates/executor-hl/src/wire.rs` | NEW. HL `/info` JSON wire structs (`InfoClearinghouseState`, `WirePosition`, `WireLeverage`, `WireOpenOrder`, `WireL2Book`, `WireMeta`, `WireUserRole` etc.). Pure data, no logic. | Create |
| `executor/crates/executor-hl/src/hl_client.rs` | Extend `AccountStateSnapshot`, add `LeverageSnapshot`, add `OpenOrder` (renamed `HlOpenOrder` to avoid collision with `executor-core::state::OpenOrder`), add `Role` enum, extend `HlClient` trait with `fetch_open_orders` / `fetch_meta` / `fetch_user_role` and an optional `dex: Option<&str>` arg on `fetch_account_state`. Implement `RealHlClient` parsing for all four endpoints. Update `MockHlClient` accordingly. | Modify |
| `executor/crates/executor-hl/src/lib.rs` | Re-export `wire` module (pub) and new public types from `hl_client`. | Modify |
| `executor/crates/executor-hl/tests/fixtures/info/` | NEW. Sanitized JSON fixtures captured from mainnet. | Create |
| `executor/crates/executor-hl/tests/parse_clearinghouse_state.rs` | NEW. Parses every fixture and asserts shape + critical fields. | Create |
| `executor/crates/executor-hl/tests/parse_open_orders.rs` | NEW. Parses openOrders / frontendOpenOrders fixtures. | Create |
| `executor/crates/executor-hl/tests/parse_l2_book.rs` | NEW. Parses an l2Book fixture. | Create |
| `executor/crates/executor-hl/tests/parse_meta_and_user_role.rs` | NEW. Parses meta and userRole fixtures. | Create |
| `executor/crates/executor-hl/tests/live_mainnet_readonly.rs` | NEW. `#[cfg(feature = "live")]` integration test that hits real mainnet (no PK, no writes). | Create |
| `executor/crates/executor-hl/Cargo.toml` | Add `live` feature flag, add `rstest` to dev-deps (already in workspace). | Modify |
| `scripts/sanitize_hl_fixture.py` | NEW. Reads a real `/tmp/hl-snapshot-*.json`, replaces sensitive fields (user address, oid, large position size details) with placeholders that still parse. | Create |
| `executor/crates/executor-hl/src/hl_client.rs` (live test infra) | Add a thin `HlInfoFetcher` helper used both by `RealHlClient` and the live test, so the live test can hit `/info` without instantiating a `Signer`. | Modify |

**Why split this way:** `wire.rs` isolates raw HL JSON shapes from domain types so future schema changes have one place to update. Tests are split per endpoint so a single fixture failure points directly to the wrong parser. The sanitizer script keeps real mainnet snapshots out of git history while preserving structural fidelity.

---

## Task 1: Workspace prep — branch + baseline build

**Files:**
- (no source changes)

- [ ] **Step 1.1: Create feature branch from develop**

```bash
cd /home/o9oem/workspace/crypto/diff-old-new
git fetch origin
git checkout develop
git pull --rebase origin develop
git checkout -b feat/pr-a-hl-readonly-parser
```

- [ ] **Step 1.2: Verify workspace builds clean**

Run: `cd executor && cargo build --workspace --all-targets`
Expected: success, no warnings (workspace lints are forbid/warn but baseline is clean per HANDOFF-2026-05-04.md).

- [ ] **Step 1.3: Verify existing tests pass**

Run: `cargo test --workspace --quiet`
Expected: 113 Rust tests pass (executor-core 22 + executor-hl 17 + executor-algo 56 + executor-server unit 8 + integration 10).

- [ ] **Step 1.4: Commit branch creation marker (no-op)**

No commit yet. Proceed to Task 2.

---

## Task 2: Sanitizer script + fixture import

**Files:**
- Create: `scripts/sanitize_hl_fixture.py`
- Create: `executor/crates/executor-hl/tests/fixtures/info/clearinghouse_state_default.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/clearinghouse_state_xyz.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/clearinghouse_state_empty.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/open_orders_xyz.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/open_orders_empty.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/frontend_open_orders_empty.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/l2_book_eth.json` (captured fresh)
- Create: `executor/crates/executor-hl/tests/fixtures/info/meta_default.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/user_role_user.json`
- Create: `executor/crates/executor-hl/tests/fixtures/info/user_role_agent.json`

- [ ] **Step 2.1: Write the sanitizer script**

Create `scripts/sanitize_hl_fixture.py`:

```python
#!/usr/bin/env python3
"""Sanitize a raw /tmp/hl-snapshot-*.json file for use as a test fixture.

Usage:
    python3 scripts/sanitize_hl_fixture.py <input.json> <output.json> <kind>

kind = clearinghouseState | openOrders | l2Book | meta | userRole

Replaces:
- user addresses -> 0x000000000000000000000000000000000000dead
- oids -> sequential 1, 2, 3, ...
- preserves all other field NAMES and TYPES so parser tests are realistic.
- preserves szi/limitPx/sz numeric STRINGS verbatim (the parser is what we test;
  changing numbers would mask formatting bugs).
"""
import json
import re
import sys
from pathlib import Path

SENTINEL_ADDR = "0x000000000000000000000000000000000000dead"


def _scrub_address(value):
    if isinstance(value, str) and re.fullmatch(r"0x[0-9a-fA-F]{40}", value):
        return SENTINEL_ADDR
    return value


def _walk(obj, oid_counter):
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            if k in ("user", "address", "deployer", "oracleUpdater", "feeRecipient"):
                out[k] = _scrub_address(v)
            elif k == "oid" and isinstance(v, int):
                out[k] = oid_counter[0]
                oid_counter[0] += 1
            elif k == "data" and isinstance(v, dict) and "user" in v:
                out[k] = {**v, "user": _scrub_address(v["user"])}
            else:
                out[k] = _walk(v, oid_counter)
        return out
    if isinstance(obj, list):
        return [_walk(x, oid_counter) for x in obj]
    return obj


def main():
    if len(sys.argv) != 4:
        print(__doc__)
        sys.exit(1)
    in_path = Path(sys.argv[1])
    out_path = Path(sys.argv[2])
    kind = sys.argv[3]
    raw = json.loads(in_path.read_text())
    oid_counter = [1]
    sanitized = _walk(raw, oid_counter)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(sanitized, indent=2, ensure_ascii=False) + "\n")
    print(f"OK: {in_path} -> {out_path} (kind={kind})")


if __name__ == "__main__":
    main()
```

- [ ] **Step 2.2: Make sanitizer executable**

Run: `chmod +x scripts/sanitize_hl_fixture.py`
Expected: no output, exit 0.

- [ ] **Step 2.3: Generate fixtures from local snapshots**

Run:
```bash
mkdir -p executor/crates/executor-hl/tests/fixtures/info
SNAP=/tmp/hl-snapshot-2026-05-05
DST=executor/crates/executor-hl/tests/fixtures/info
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_clearinghouseState.json"      "$DST/clearinghouse_state_default.json"      clearinghouseState
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_clearinghouseState_xyz.json"  "$DST/clearinghouse_state_xyz.json"          clearinghouseState
python3 scripts/sanitize_hl_fixture.py "$SNAP/clearinghouseState.json"             "$DST/clearinghouse_state_empty.json"        clearinghouseState
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_openOrders_xyz.json"          "$DST/open_orders_xyz.json"                  openOrders
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_openOrders.json"              "$DST/open_orders_empty.json"                openOrders
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_frontendOpenOrders.json"      "$DST/frontend_open_orders_empty.json"       openOrders
python3 scripts/sanitize_hl_fixture.py "$SNAP/meta.json"                           "$DST/meta_default.json"                     meta
python3 scripts/sanitize_hl_fixture.py "$SNAP/master_userRole.json"                "$DST/user_role_user.json"                   userRole
python3 scripts/sanitize_hl_fixture.py "$SNAP/userRole.json"                       "$DST/user_role_agent.json"                  userRole
ls "$DST"
```

Expected: 9 files listed, each starts with `{` or `[`.

- [ ] **Step 2.4: Capture a fresh l2Book ETH fixture (read-only mainnet)**

Run:
```bash
DST=executor/crates/executor-hl/tests/fixtures/info
curl -sf -X POST https://api.hyperliquid.xyz/info \
  -H 'Content-Type: application/json' \
  -d '{"type":"l2Book","coin":"ETH"}' \
  | python3 -m json.tool > "$DST/l2_book_eth.json"
head -c 200 "$DST/l2_book_eth.json"
```

Expected: starts with `{ "coin": "ETH", "time": ..., "levels": [ [ {"px": "..."`. No address fields, so no sanitization needed.

- [ ] **Step 2.5: Verify fixtures contain no real master/agent addresses**

Run:
```bash
DST=executor/crates/executor-hl/tests/fixtures/info
grep -rE '0x[0-9a-fA-F]{40}' "$DST" | grep -vE '0x000000000000000000000000000000000000dead' || echo "CLEAN: no live addresses"
```

Expected: `CLEAN: no live addresses` (the sanitizer should have scrubbed all 0x... 40-hex-char strings to the dead sentinel).

- [ ] **Step 2.6: Commit fixtures + sanitizer**

```bash
git add scripts/sanitize_hl_fixture.py executor/crates/executor-hl/tests/fixtures/
git commit -m "test(executor-hl): add sanitized HL /info fixtures + sanitizer script"
```

---

## Task 3: Wire types — `clearinghouseState` parsing

**Files:**
- Create: `executor/crates/executor-hl/src/wire.rs`
- Create: `executor/crates/executor-hl/tests/parse_clearinghouse_state.rs`
- Modify: `executor/crates/executor-hl/src/lib.rs` (add `pub mod wire;`)

- [ ] **Step 3.1: Write the failing test for clearinghouseState parsing**

Create `executor/crates/executor-hl/tests/parse_clearinghouse_state.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{InfoClearinghouseState, WireLeverageType};
use rust_decimal_macros::dec;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_default_dex_with_one_position() {
    let json = fixture("clearinghouse_state_default.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse default");

    // Top-level
    assert_eq!(s.margin_summary.account_value, dec!(643.718581));
    assert_eq!(s.margin_summary.total_margin_used, dec!(608.847078));
    assert_eq!(s.margin_summary.total_ntl_pos, dec!(6088.47078));
    assert_eq!(s.margin_summary.total_raw_usd, dec!(-5444.752199));
    assert_eq!(s.cross_maintenance_margin_used, dec!(304.423539));
    assert_eq!(s.withdrawable, dec!(34.871503));
    assert_eq!(s.time, 1777951400108_u64);

    // Positions
    assert_eq!(s.asset_positions.len(), 1);
    let p = &s.asset_positions[0].position;
    assert_eq!(p.coin, "HYPE");
    assert_eq!(p.szi, dec!(144.53));
    assert_eq!(p.entry_px, dec!(41.5108));
    assert_eq!(p.leverage.leverage_type, WireLeverageType::Cross);
    assert_eq!(p.leverage.value, 10);
    assert_eq!(p.position_value, dec!(6088.47078));
    assert_eq!(p.unrealized_pnl, dec!(88.91306));
    assert_eq!(p.liquidation_px.unwrap(), dec!(27.0268023758));
    assert_eq!(p.margin_used, dec!(608.847078));
    assert_eq!(p.max_leverage, 10);
}

#[test]
fn parses_xyz_dex_with_one_position() {
    let json = fixture("clearinghouse_state_xyz.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse xyz");
    assert_eq!(s.asset_positions.len(), 1);
    assert_eq!(s.asset_positions[0].position.coin, "xyz:META");
    assert_eq!(s.asset_positions[0].position.szi, dec!(3.262));
}

#[test]
fn parses_empty_account() {
    let json = fixture("clearinghouse_state_empty.json");
    let s: InfoClearinghouseState = serde_json::from_str(&json).expect("parse empty");
    assert_eq!(s.asset_positions.len(), 0);
    assert_eq!(s.withdrawable, dec!(0));
    assert_eq!(s.margin_summary.account_value, dec!(0));
}
```

- [ ] **Step 3.2: Run test to verify it fails (no `wire` module yet)**

Run: `cd executor && cargo test -p executor-hl --test parse_clearinghouse_state 2>&1 | head -30`
Expected: compile error `unresolved import executor_hl::wire`.

- [ ] **Step 3.3: Implement `wire.rs` with `InfoClearinghouseState` and friends**

Create `executor/crates/executor-hl/src/wire.rs`:

```rust
//! HL `/info` endpoint JSON wire types.
//!
//! These structs mirror the on-the-wire shape exactly (camelCase JSON,
//! string-encoded numerics). Domain mapping into `executor_core::state`
//! lives in `hl_client.rs`.
//!
//! All numeric fields use `#[serde(with = "rust_decimal::serde::str")]`
//! because HL serializes prices/sizes as JSON strings to preserve precision.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// HL `clearinghouseState` response (perp).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InfoClearinghouseState {
    #[serde(rename = "marginSummary")]
    pub margin_summary: WireMarginSummary,
    #[serde(rename = "crossMarginSummary")]
    pub cross_margin_summary: WireMarginSummary,
    #[serde(rename = "crossMaintenanceMarginUsed", with = "rust_decimal::serde::str")]
    pub cross_maintenance_margin_used: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub withdrawable: Decimal,
    #[serde(rename = "assetPositions")]
    pub asset_positions: Vec<WireAssetPosition>,
    pub time: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireMarginSummary {
    #[serde(rename = "accountValue", with = "rust_decimal::serde::str")]
    pub account_value: Decimal,
    #[serde(rename = "totalNtlPos", with = "rust_decimal::serde::str")]
    pub total_ntl_pos: Decimal,
    #[serde(rename = "totalRawUsd", with = "rust_decimal::serde::str")]
    pub total_raw_usd: Decimal,
    #[serde(rename = "totalMarginUsed", with = "rust_decimal::serde::str")]
    pub total_margin_used: Decimal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireAssetPosition {
    #[serde(rename = "type")]
    pub position_type: String, // "oneWay" today
    pub position: WirePosition,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WirePosition {
    pub coin: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub szi: Decimal,
    pub leverage: WireLeverage,
    #[serde(rename = "entryPx", with = "rust_decimal::serde::str")]
    pub entry_px: Decimal,
    #[serde(rename = "positionValue", with = "rust_decimal::serde::str")]
    pub position_value: Decimal,
    #[serde(rename = "unrealizedPnl", with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    #[serde(rename = "returnOnEquity", with = "rust_decimal::serde::str")]
    pub return_on_equity: Decimal,
    #[serde(rename = "liquidationPx", default, with = "rust_decimal::serde::str_option")]
    pub liquidation_px: Option<Decimal>,
    #[serde(rename = "marginUsed", with = "rust_decimal::serde::str")]
    pub margin_used: Decimal,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "cumFunding")]
    pub cum_funding: WireCumFunding,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireLeverage {
    #[serde(rename = "type")]
    pub leverage_type: WireLeverageType,
    pub value: u32,
    #[serde(rename = "rawUsd", default, with = "rust_decimal::serde::str_option")]
    pub raw_usd: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireLeverageType {
    Cross,
    Isolated,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireCumFunding {
    #[serde(rename = "allTime", with = "rust_decimal::serde::str")]
    pub all_time: Decimal,
    #[serde(rename = "sinceOpen", with = "rust_decimal::serde::str")]
    pub since_open: Decimal,
    #[serde(rename = "sinceChange", with = "rust_decimal::serde::str")]
    pub since_change: Decimal,
}
```

- [ ] **Step 3.4: Add `pub mod wire;` to `lib.rs`**

Edit `executor/crates/executor-hl/src/lib.rs`. Find the existing `pub mod` declarations near the top and add:

```rust
pub mod wire;
```

(Keep the existing modules untouched.)

- [ ] **Step 3.5: Run test to verify it passes**

Run: `cd executor && cargo test -p executor-hl --test parse_clearinghouse_state -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 3.6: Run clippy to verify no warnings**

Run: `cd executor && cargo clippy -p executor-hl --all-targets -- -D warnings 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 3.7: Commit wire structs + clearinghouseState parsing**

```bash
git add executor/crates/executor-hl/src/wire.rs \
        executor/crates/executor-hl/src/lib.rs \
        executor/crates/executor-hl/tests/parse_clearinghouse_state.rs
git commit -m "feat(executor-hl): wire structs + clearinghouseState parser"
```

---

## Task 4: Wire types — `openOrders` parsing

**Files:**
- Modify: `executor/crates/executor-hl/src/wire.rs`
- Create: `executor/crates/executor-hl/tests/parse_open_orders.rs`

- [ ] **Step 4.1: Write the failing test for openOrders parsing**

Create `executor/crates/executor-hl/tests/parse_open_orders.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{WireFrontendOpenOrder, WireOpenOrder, WireOrderSide};
use rust_decimal_macros::dec;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_open_orders_xyz_googl() {
    let json = fixture("open_orders_xyz.json");
    let v: Vec<WireOpenOrder> = serde_json::from_str(&json).expect("parse xyz orders");
    assert_eq!(v.len(), 1);
    let o = &v[0];
    assert_eq!(o.coin, "xyz:GOOGL");
    assert_eq!(o.side, WireOrderSide::B);
    assert!(o.limit_px > dec!(0));
    assert!(o.sz > dec!(0));
    assert!(o.timestamp > 0);
}

#[test]
fn parses_empty_open_orders() {
    let json = fixture("open_orders_empty.json");
    let v: Vec<WireOpenOrder> = serde_json::from_str(&json).expect("parse empty");
    assert_eq!(v.len(), 0);
}

#[test]
fn parses_empty_frontend_open_orders() {
    let json = fixture("frontend_open_orders_empty.json");
    let v: Vec<WireFrontendOpenOrder> = serde_json::from_str(&json).expect("parse empty fe");
    assert_eq!(v.len(), 0);
}
```

- [ ] **Step 4.2: Run test to verify it fails (no openOrders types yet)**

Run: `cd executor && cargo test -p executor-hl --test parse_open_orders 2>&1 | head -10`
Expected: compile error `WireOpenOrder` etc not found.

- [ ] **Step 4.3: Add openOrders / frontendOpenOrders types to `wire.rs`**

Append to `executor/crates/executor-hl/src/wire.rs`:

```rust
/// HL `openOrders` array element.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireOpenOrder {
    pub coin: String,
    pub side: WireOrderSide,
    #[serde(rename = "limitPx", with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub oid: u64,
    /// HL wire is ms epoch (number).
    pub timestamp: i64,
}

/// HL `frontendOpenOrders` array element (superset of openOrders).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireFrontendOpenOrder {
    pub coin: String,
    pub side: WireOrderSide,
    #[serde(rename = "limitPx", with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub oid: u64,
    pub timestamp: i64,
    /// Order type label, e.g. "Limit", "Trigger". Free string in spec.
    #[serde(rename = "orderType")]
    pub order_type: String,
    #[serde(rename = "origSz", with = "rust_decimal::serde::str")]
    pub orig_sz: Decimal,
    #[serde(rename = "reduceOnly", default)]
    pub reduce_only: bool,
    #[serde(rename = "isTrigger", default)]
    pub is_trigger: bool,
    #[serde(rename = "isPositionTpsl", default)]
    pub is_position_tpsl: bool,
    #[serde(
        rename = "triggerPx",
        default,
        with = "rust_decimal::serde::str_option"
    )]
    pub trigger_px: Option<Decimal>,
    #[serde(rename = "triggerCondition", default)]
    pub trigger_condition: Option<String>,
}

/// HL wire side: A = ask = sell, B = bid = buy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum WireOrderSide {
    A,
    B,
}
```

- [ ] **Step 4.4: Run test to verify it passes**

Run: `cd executor && cargo test -p executor-hl --test parse_open_orders -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 4.5: Commit**

```bash
git add executor/crates/executor-hl/src/wire.rs executor/crates/executor-hl/tests/parse_open_orders.rs
git commit -m "feat(executor-hl): openOrders + frontendOpenOrders wire types"
```

---

## Task 5: Wire types — `l2Book` parsing + mapping into `OrderBook`

**Files:**
- Modify: `executor/crates/executor-hl/src/wire.rs`
- Create: `executor/crates/executor-hl/tests/parse_l2_book.rs`

- [ ] **Step 5.1: Write the failing test for l2Book parsing**

Create `executor/crates/executor-hl/tests/parse_l2_book.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::state::OrderBook;
use executor_hl::wire::WireL2Book;
use rust_decimal::Decimal;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_l2_book_eth_levels() {
    let json = fixture("l2_book_eth.json");
    let book: WireL2Book = serde_json::from_str(&json).expect("parse l2 book");
    assert_eq!(book.coin, "ETH");
    assert_eq!(book.levels.len(), 2);
    assert!(!book.levels[0].is_empty(), "bids should not be empty");
    assert!(!book.levels[1].is_empty(), "asks should not be empty");

    // bids descending: levels[0][0].px > levels[0][1].px
    if book.levels[0].len() >= 2 {
        assert!(book.levels[0][0].px > book.levels[0][1].px, "bids desc");
    }
    // asks ascending
    if book.levels[1].len() >= 2 {
        assert!(book.levels[1][0].px < book.levels[1][1].px, "asks asc");
    }

    // best bid < best ask
    let best_bid = book.levels[0][0].px;
    let best_ask = book.levels[1][0].px;
    assert!(best_bid < best_ask, "spread positive");
}

#[test]
fn maps_into_executor_core_orderbook() {
    let json = fixture("l2_book_eth.json");
    let wire: WireL2Book = serde_json::from_str(&json).unwrap();
    let book: OrderBook = wire.to_orderbook();
    assert!(book.best_bid().unwrap() > Decimal::ZERO);
    assert!(book.best_ask().unwrap() > book.best_bid().unwrap());
    assert!(book.ts.is_some());
}
```

- [ ] **Step 5.2: Run test to verify it fails**

Run: `cd executor && cargo test -p executor-hl --test parse_l2_book 2>&1 | head -15`
Expected: compile error.

- [ ] **Step 5.3: Add `WireL2Book` and `to_orderbook()` to `wire.rs`**

Append to `executor/crates/executor-hl/src/wire.rs`:

```rust
use chrono::{DateTime, TimeZone, Utc};
use executor_core::state::{BookLevel, OrderBook};

/// HL `l2Book` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireL2Book {
    pub coin: String,
    /// ms epoch of snapshot.
    pub time: i64,
    /// Two arrays: \[bids, asks\]. Bids descending, asks ascending.
    pub levels: Vec<Vec<WireBookLevel>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireBookLevel {
    #[serde(with = "rust_decimal::serde::str")]
    pub px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub n: u32,
}

impl From<&WireBookLevel> for BookLevel {
    fn from(w: &WireBookLevel) -> Self {
        Self {
            px: w.px,
            sz: w.sz,
            n: w.n,
        }
    }
}

impl WireL2Book {
    /// Map into the domain `OrderBook` used by algorithms.
    ///
    /// HL guarantees `levels` is `[bids, asks]` with bids descending and
    /// asks ascending; if a malformed response arrives with fewer than two
    /// arrays, both sides are emitted empty so callers see "no quotes" rather
    /// than panicking.
    pub fn to_orderbook(&self) -> OrderBook {
        let bids = self
            .levels
            .first()
            .map(|v| v.iter().map(BookLevel::from).collect())
            .unwrap_or_default();
        let asks = self
            .levels
            .get(1)
            .map(|v| v.iter().map(BookLevel::from).collect())
            .unwrap_or_default();
        OrderBook {
            bids,
            asks,
            ts: Utc.timestamp_millis_opt(self.time).single(),
        }
    }
}
```

(Note: `chrono::TimeZone` and `executor_core::state::{BookLevel, OrderBook}` go at the top of the file with the other use statements; keep them alongside the existing `serde::{Deserialize, Serialize}` block.)

- [ ] **Step 5.4: Move the new use statements to the top of `wire.rs`**

Edit `executor/crates/executor-hl/src/wire.rs`. Make sure the very top of the file looks like:

```rust
//! HL `/info` endpoint JSON wire types.
//!
//! ... (keep existing module doc)

use chrono::{DateTime, TimeZone, Utc};
use executor_core::state::{BookLevel, OrderBook};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
```

(Remove the duplicate inline `use` statements you wrote in Step 5.3; only one block at the top of the file.)

- [ ] **Step 5.5: Run test to verify it passes**

Run: `cd executor && cargo test -p executor-hl --test parse_l2_book -- --nocapture`
Expected: 2 tests pass.

- [ ] **Step 5.6: Re-run all parser tests as a regression check**

Run: `cd executor && cargo test -p executor-hl --test parse_clearinghouse_state --test parse_open_orders --test parse_l2_book`
Expected: 8 tests pass total (3 + 3 + 2).

- [ ] **Step 5.7: Commit**

```bash
git add executor/crates/executor-hl/src/wire.rs executor/crates/executor-hl/tests/parse_l2_book.rs
git commit -m "feat(executor-hl): l2Book wire type + OrderBook mapping"
```

---

## Task 6: Wire types — `meta` and `userRole` parsing

**Files:**
- Modify: `executor/crates/executor-hl/src/wire.rs`
- Create: `executor/crates/executor-hl/tests/parse_meta_and_user_role.rs`

- [ ] **Step 6.1: Write the failing test**

Create `executor/crates/executor-hl/tests/parse_meta_and_user_role.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_hl::wire::{WireMeta, WireUserRole};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/info");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"))
}

#[test]
fn parses_meta_default_universe() {
    let json = fixture("meta_default.json");
    let m: WireMeta = serde_json::from_str(&json).expect("parse meta");
    assert!(m.universe.len() >= 200, "expect 200+ perps");
    let btc = m.universe.iter().find(|u| u.name == "BTC").expect("BTC present");
    assert_eq!(btc.sz_decimals, 5);
    assert_eq!(btc.max_leverage, 40);
    assert!(!btc.only_isolated);
    let eth = m.universe.iter().find(|u| u.name == "ETH").expect("ETH present");
    assert_eq!(eth.sz_decimals, 4);
    assert_eq!(eth.max_leverage, 25);
}

#[test]
fn parses_user_role_user() {
    let json = fixture("user_role_user.json");
    let r: WireUserRole = serde_json::from_str(&json).expect("parse role user");
    match r {
        WireUserRole::User => {}
        other => panic!("expected User, got {other:?}"),
    }
}

#[test]
fn parses_user_role_agent() {
    let json = fixture("user_role_agent.json");
    let r: WireUserRole = serde_json::from_str(&json).expect("parse role agent");
    match r {
        WireUserRole::Agent { data } => {
            assert!(data.user.starts_with("0x"));
        }
        other => panic!("expected Agent, got {other:?}"),
    }
}
```

- [ ] **Step 6.2: Run test to verify it fails**

Run: `cd executor && cargo test -p executor-hl --test parse_meta_and_user_role 2>&1 | head -10`
Expected: compile error.

- [ ] **Step 6.3: Add `WireMeta` and `WireUserRole` to `wire.rs`**

Append to `executor/crates/executor-hl/src/wire.rs`:

```rust
/// HL `meta` response (perp universe).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireMeta {
    pub universe: Vec<WireUniverseEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireUniverseEntry {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u32,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "onlyIsolated", default)]
    pub only_isolated: bool,
}

/// HL `userRole` response.
///
/// Wire shapes (from real responses):
/// - `{"role":"user"}`
/// - `{"role":"agent","data":{"user":"0x..."}}`
/// - `{"role":"vault"}` / `"subAccount"` / `"missing"` (data optional)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum WireUserRole {
    User,
    Agent {
        data: WireAgentData,
    },
    Vault,
    SubAccount,
    Missing,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireAgentData {
    pub user: String,
}
```

- [ ] **Step 6.4: Run test to verify it passes**

Run: `cd executor && cargo test -p executor-hl --test parse_meta_and_user_role -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 6.5: Run all parser tests + clippy**

Run:
```bash
cd executor
cargo test -p executor-hl --tests
cargo clippy -p executor-hl --all-targets -- -D warnings
```
Expected: 11 parser tests pass (3+3+2+3) plus existing 17 unit tests; clippy clean.

- [ ] **Step 6.6: Commit**

```bash
git add executor/crates/executor-hl/src/wire.rs executor/crates/executor-hl/tests/parse_meta_and_user_role.rs
git commit -m "feat(executor-hl): meta + userRole wire types"
```

---

## Task 7: Domain mapping — extend `AccountStateSnapshot` + `HlClient` trait

**Files:**
- Modify: `executor/crates/executor-hl/src/hl_client.rs`
- Modify: `executor/crates/executor-hl/src/lib.rs`

- [ ] **Step 7.1: Write the failing test for `AccountStateSnapshot::from_wire`**

Append to `executor/crates/executor-hl/tests/parse_clearinghouse_state.rs`:

```rust
use executor_core::types::Address;
use executor_core::symbol::Symbol;
use executor_hl::AccountStateSnapshot;

#[test]
fn maps_wire_into_account_state_snapshot() {
    let json = fixture("clearinghouse_state_default.json");
    let wire: InfoClearinghouseState = serde_json::from_str(&json).unwrap();

    let addr = Address::new("0x000000000000000000000000000000000000dead");
    let snap = AccountStateSnapshot::from_wire(addr.clone(), &wire);

    assert_eq!(snap.address, addr);
    assert_eq!(snap.account_value, dec!(643.718581));
    assert_eq!(snap.margin_used, dec!(608.847078));
    assert_eq!(snap.withdrawable, dec!(34.871503));
    assert_eq!(snap.cross_maintenance_margin_used, dec!(304.423539));
    assert_eq!(snap.positions.len(), 1);
    let hype = snap.positions.get(&Symbol::new("HYPE")).expect("HYPE");
    assert_eq!(hype.size, dec!(144.53));
    assert_eq!(hype.entry_px, Some(dec!(41.5108)));
    assert_eq!(hype.unrealized_pnl, Some(dec!(88.91306)));
    assert_eq!(hype.margin_used, Some(dec!(608.847078)));
}
```

- [ ] **Step 7.2: Run test to verify it fails**

Run: `cd executor && cargo test -p executor-hl --test parse_clearinghouse_state maps_wire 2>&1 | head -10`
Expected: compile error — `address`, `account_value`, `withdrawable`, `cross_maintenance_margin_used`, `from_wire` don't exist on `AccountStateSnapshot`.

- [ ] **Step 7.3: Extend `AccountStateSnapshot` and add `from_wire`**

Edit `executor/crates/executor-hl/src/hl_client.rs`. Replace the existing `AccountStateSnapshot` definition (around line 49) with:

```rust
/// Account-level snapshot returned by `/info clearinghouseState`.
///
/// HL returns numeric values as JSON strings; this struct holds them as
/// `Decimal` after `from_wire` mapping. `address` is the master EOA the
/// snapshot was fetched for; `server_time` is HL's snapshot timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStateSnapshot {
    pub address: Address,
    pub margin_used: Decimal,
    pub account_value: Decimal,
    pub withdrawable: Decimal,
    pub cross_maintenance_margin_used: Decimal,
    pub positions: HashMap<Symbol, Position>,
    pub open_orders_by_cloid: HashMap<Cloid, OrderId>,
    pub fetched_at: DateTime<Utc>,
    pub server_time: DateTime<Utc>,
}

impl AccountStateSnapshot {
    /// Map the wire representation into the domain snapshot.
    pub fn from_wire(address: Address, wire: &crate::wire::InfoClearinghouseState) -> Self {
        let now = Utc::now();
        let server_time =
            chrono::Utc.timestamp_millis_opt(wire.time as i64).single().unwrap_or(now);
        let mut positions = HashMap::new();
        for ap in &wire.asset_positions {
            let p = &ap.position;
            positions.insert(
                Symbol::new(&p.coin),
                Position {
                    size: p.szi,
                    entry_px: Some(p.entry_px),
                    unrealized_pnl: Some(p.unrealized_pnl),
                    margin_used: Some(p.margin_used),
                    last_update: Some(server_time),
                },
            );
        }
        Self {
            address,
            margin_used: wire.margin_summary.total_margin_used,
            account_value: wire.margin_summary.account_value,
            withdrawable: wire.withdrawable,
            cross_maintenance_margin_used: wire.cross_maintenance_margin_used,
            positions,
            open_orders_by_cloid: HashMap::new(),
            fetched_at: now,
            server_time,
        }
    }
}
```

Add at the top of the file (with the existing `use` block):

```rust
use chrono::TimeZone;
```

(Keep all other existing `use` lines.)

- [ ] **Step 7.4: Update `MockHlClient::seed_account` callers — none should break**

Run: `cd executor && cargo build -p executor-hl --tests 2>&1 | head -30`
If any existing test instantiates `AccountStateSnapshot { ... }` with the old fields, those break: `Default` is no longer derived on the struct because `Address` doesn't impl `Default`. Search for direct constructions:

Run: `cd executor && grep -rn 'AccountStateSnapshot {' crates/ tests/ 2>&1 | grep -v 'src/hl_client.rs'`

For every hit, update the literal to include the new fields. Most likely there are 0–1 hits. If `MockHlClient::seed_account` callers used `AccountStateSnapshot::default()`, replace with an explicit constructor:

Add to `hl_client.rs` (just below the impl block):

```rust
impl AccountStateSnapshot {
    /// Empty snapshot for tests/mocks where address may not be known yet.
    pub fn empty(address: Address) -> Self {
        let now = Utc::now();
        Self {
            address,
            margin_used: Decimal::ZERO,
            account_value: Decimal::ZERO,
            withdrawable: Decimal::ZERO,
            cross_maintenance_margin_used: Decimal::ZERO,
            positions: HashMap::new(),
            open_orders_by_cloid: HashMap::new(),
            fetched_at: now,
            server_time: now,
        }
    }
}
```

Replace the `Default` derivation on `AccountStateSnapshot` (it currently has `#[derive(Debug, Clone, Default, ...)]` — remove `Default`).

In the same file, find `MockHlClient` and update `account` field type if it was `Mutex<AccountStateSnapshot>`. Default value is no longer available; change initializer:

```rust
// in MockHlClient default / new
account: Mutex::new(AccountStateSnapshot::empty(Address::new("0x0000000000000000000000000000000000000000"))),
```

(Adjust to what the file already does — preserve existing patterns; this is the minimal change.)

- [ ] **Step 7.5: Run all executor-hl tests**

Run: `cd executor && cargo test -p executor-hl`
Expected: all unit + parser tests green (17 + 12 = 29).

- [ ] **Step 7.6: Re-export `AccountStateSnapshot` from `lib.rs`**

Edit `executor/crates/executor-hl/src/lib.rs`. Find the existing `pub use hl_client::*;` (or add one if not present) so external crates and tests can `use executor_hl::AccountStateSnapshot;`. If there is already a `pub use`, ensure `AccountStateSnapshot` is in the re-export list.

- [ ] **Step 7.7: Build the whole workspace to catch downstream breakage**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | tail -20`
Expected: clean build. If `executor-server` or `executor-algo` reference `AccountStateSnapshot::default()`, fix using `::empty(...)` similarly.

- [ ] **Step 7.8: Commit**

```bash
git add executor/crates/executor-hl/src/hl_client.rs \
        executor/crates/executor-hl/src/lib.rs \
        executor/crates/executor-hl/tests/parse_clearinghouse_state.rs
git commit -m "feat(executor-hl): AccountStateSnapshot extension + from_wire mapping"
```

---

## Task 8: HlClient trait surface — add `dex` arg + new endpoints

**Files:**
- Modify: `executor/crates/executor-hl/src/hl_client.rs`

- [ ] **Step 8.1: Define `HlOpenOrder` and `Role` domain types**

Add to `executor/crates/executor-hl/src/hl_client.rs` (below `OrderResponse`):

```rust
/// Domain-level open order (one HL `openOrders` entry mapped to local types).
///
/// We keep the wire `oid` and translate side into `executor_core::types::Side`
/// for callers; cloid is unknown from `openOrders` alone (HL does not echo it
/// in this endpoint), so it stays None until matched with local registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlOpenOrder {
    pub symbol: Symbol,
    pub side: executor_core::types::Side,
    pub limit_px: Decimal,
    pub sz: Decimal,
    pub oid: OrderId,
    pub timestamp: DateTime<Utc>,
}

impl HlOpenOrder {
    pub fn from_wire(w: &crate::wire::WireOpenOrder) -> Self {
        use crate::wire::WireOrderSide;
        let side = match w.side {
            WireOrderSide::A => executor_core::types::Side::Short, // ask = sell
            WireOrderSide::B => executor_core::types::Side::Long,  // bid = buy
        };
        let ts = chrono::Utc
            .timestamp_millis_opt(w.timestamp)
            .single()
            .unwrap_or_else(Utc::now);
        Self {
            symbol: Symbol::new(&w.coin),
            side,
            limit_px: w.limit_px,
            sz: w.sz,
            oid: OrderId(w.oid),
            timestamp: ts,
        }
    }
}

/// HL `userRole` mapped to a Rust enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Agent { master: Address },
    Vault,
    SubAccount,
    Missing,
}

impl Role {
    pub fn from_wire(w: &crate::wire::WireUserRole) -> Self {
        use crate::wire::WireUserRole as W;
        match w {
            W::User => Role::User,
            W::Agent { data } => Role::Agent {
                master: Address::new(&data.user),
            },
            W::Vault => Role::Vault,
            W::SubAccount => Role::SubAccount,
            W::Missing => Role::Missing,
        }
    }
}
```

- [ ] **Step 8.2: Extend the `HlClient` trait**

Edit the existing `HlClient` trait in `executor/crates/executor-hl/src/hl_client.rs`. Replace it with:

```rust
#[async_trait]
pub trait HlClient: Send + Sync {
    /// Fetch full account state. `dex=None` selects the default perp dex;
    /// `dex=Some("xyz")` etc. selects a HIP-3 builder dex.
    async fn fetch_account_state(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<AccountStateSnapshot, HlError>;

    /// Fetch a fresh top-N book snapshot.
    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError>;

    /// Fetch all open orders for the address (default dex unless specified).
    async fn fetch_open_orders(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<Vec<HlOpenOrder>, HlError>;

    /// Fetch the perp universe metadata (symbol list + leverage caps).
    async fn fetch_meta(&self, dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError>;

    /// Identify the role of an address (catches agent/master mix-ups).
    async fn fetch_user_role(&self, address: &Address) -> Result<Role, HlError>;

    /// Place a batch of orders.
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError>;

    /// Cancel a batch of orders.
    async fn cancel_orders(&self, cancels: &[CancelIntent]) -> Result<Vec<OrderResponse>, HlError>;
}
```

- [ ] **Step 8.3: Update `MockHlClient` to satisfy the new trait**

Find `impl HlClient for MockHlClient` in the same file. Update the `fetch_account_state` signature to include `_dex: Option<&str>`, and add three new method bodies:

```rust
async fn fetch_account_state(
    &self,
    _address: &Address,
    _dex: Option<&str>,
) -> Result<AccountStateSnapshot, HlError> {
    let snap = self
        .account
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| AccountStateSnapshot::empty(Address::new("0x0")));
    Ok(snap)
}

async fn fetch_open_orders(
    &self,
    _address: &Address,
    _dex: Option<&str>,
) -> Result<Vec<HlOpenOrder>, HlError> {
    Ok(Vec::new())
}

async fn fetch_meta(&self, _dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError> {
    Ok(crate::wire::WireMeta { universe: Vec::new() })
}

async fn fetch_user_role(&self, _address: &Address) -> Result<Role, HlError> {
    Ok(Role::User)
}
```

(Keep `fetch_book_snapshot`, `place_orders`, `cancel_orders` unchanged.)

- [ ] **Step 8.4: Update `RealHlClient` impl — add the 3 new methods + change `fetch_account_state` signature**

Find `impl HlClient for RealHlClient`. Replace it with:

```rust
#[async_trait]
impl HlClient for RealHlClient {
    async fn fetch_account_state(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<AccountStateSnapshot, HlError> {
        let _wait = self.rate_limiter.acquire(2).await;
        let mut body = serde_json::json!({
            "type": "clearinghouseState",
            "user": address.as_str(),
        });
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::InfoClearinghouseState = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("clearinghouseState: {e}")))?;
        Ok(AccountStateSnapshot::from_wire(address.clone(), &wire))
    }

    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError> {
        let _wait = self.rate_limiter.acquire(2).await;
        let body = serde_json::json!({
            "type": "l2Book",
            "coin": symbol.as_str(),
        });
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::WireL2Book = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("l2Book: {e}")))?;
        Ok(wire.to_orderbook())
    }

    async fn fetch_open_orders(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<Vec<HlOpenOrder>, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let mut body = serde_json::json!({
            "type": "openOrders",
            "user": address.as_str(),
        });
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        let wire: Vec<crate::wire::WireOpenOrder> = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("openOrders: {e}")))?;
        Ok(wire.iter().map(HlOpenOrder::from_wire).collect())
    }

    async fn fetch_meta(&self, dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let mut body = serde_json::json!({"type": "meta"});
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("meta: {e}")))
    }

    async fn fetch_user_role(&self, address: &Address) -> Result<Role, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let body = serde_json::json!({
            "type": "userRole",
            "user": address.as_str(),
        });
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::WireUserRole = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("userRole: {e}")))?;
        Ok(Role::from_wire(&wire))
    }

    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        // Unchanged 80% scaffold — kept as-is until PR-B implements signing.
        if orders.is_empty() {
            return Ok(vec![]);
        }
        let _wait = self.rate_limiter.acquire(orders.len() as u32).await;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let action = serde_json::json!({"type": "order", "orders": orders.len()});
        let _sig = self.signer.sign_l1(&action, nonce).await?;
        Err(HlError::Exchange {
            code: Some("not_implemented".into()),
            message: "RealHlClient::place_orders is the 80% scaffold; \
                      full HL signing arrives with the key-management PR"
                .into(),
        })
    }

    async fn cancel_orders(
        &self,
        _cancels: &[CancelIntent],
    ) -> Result<Vec<OrderResponse>, HlError> {
        Err(HlError::Exchange {
            code: Some("not_implemented".into()),
            message: "RealHlClient::cancel_orders implemented in PR-B".into(),
        })
    }
}
```

Add a private helper `post_info` on `RealHlClient` (just above the impl block):

```rust
impl RealHlClient {
    /// POST a JSON body to the /info endpoint and return the response body as a String.
    /// Maps non-2xx into `HlError::Network`.
    async fn post_info(&self, body: &serde_json::Value) -> Result<String, HlError> {
        let resp = self
            .http
            .post(&self.config.info_url)
            .json(body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(HlError::Network(format!("HTTP {}", resp.status())));
        }
        resp.text().await.map_err(|e| HlError::Network(e.to_string()))
    }
}
```

- [ ] **Step 8.5: Find every existing call site and add `None` for the new `dex` parameter**

Run: `cd executor && cargo build --workspace --all-targets 2>&1 | grep -E '^error' | head -30`

For every `fetch_account_state(addr)` call, change to `fetch_account_state(addr, None)`. Most likely call sites: `executor-server/src/state.rs`, `executor-server/src/routes.rs`, integration tests under `executor-server/tests/`.

- [ ] **Step 8.6: Run the full workspace test suite**

Run: `cd executor && cargo test --workspace`
Expected: all 113+ tests pass (existing 113 + 12 new parser tests = 125+).

- [ ] **Step 8.7: Run clippy across the workspace**

Run: `cd executor && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

- [ ] **Step 8.8: Commit**

```bash
git add executor/crates/executor-hl/src/hl_client.rs
# plus whatever call sites you had to fix
git add -u  # for any modified call-site files
git commit -m "feat(executor-hl): HlClient trait — add fetch_open_orders/meta/user_role + dex arg"
```

---

## Task 9: Live mainnet read-only integration test (feature-gated)

**Files:**
- Modify: `executor/crates/executor-hl/Cargo.toml`
- Create: `executor/crates/executor-hl/tests/live_mainnet_readonly.rs`

- [ ] **Step 9.1: Add a `live` feature flag**

Edit `executor/crates/executor-hl/Cargo.toml`. After the `[dependencies]` block (or replace any existing `[features]` block), add:

```toml
[features]
default = []
# Opt-in: hits real HL mainnet /info (read-only, no PK, no writes).
# Skipped by default in CI. Enable with: cargo test -p executor-hl --features live
live = []
```

- [ ] **Step 9.2: Write the live integration test**

Create `executor/crates/executor-hl/tests/live_mainnet_readonly.rs`:

```rust
//! Live HL mainnet read-only integration test.
//!
//! Hits the real /info endpoint with no PK and no writes. Skipped by default;
//! enable with: `cargo test -p executor-hl --features live -- --nocapture`.
//!
//! Uses `HL_TEST_ADDRESS` env var (defaults to a public read-only address)
//! so we don't bake a specific master EOA into git history.

#![cfg(feature = "live")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::types::Address;
use executor_core::symbol::Symbol;
use executor_hl::hl_client::{HlClient, HlConfig, RealHlClient, Role};
use executor_hl::signer::MockSigner;
use std::sync::Arc;

fn test_address() -> Address {
    // env > default. Public address; no auth required to query.
    let s = std::env::var("HL_TEST_ADDRESS")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000dead".into());
    Address::new(s)
}

fn client() -> RealHlClient {
    let signer = Arc::new(MockSigner::new());
    RealHlClient::new(HlConfig::mainnet(), signer)
}

#[tokio::test]
async fn live_fetch_user_role_returns_a_known_variant() {
    let c = client();
    let addr = test_address();
    let role = c.fetch_user_role(&addr).await.expect("fetch role");
    // Any of these is valid; we just assert the call round-trips.
    assert!(matches!(
        role,
        Role::User | Role::Agent { .. } | Role::Vault | Role::SubAccount | Role::Missing
    ));
    eprintln!("live role for {addr}: {role:?}");
}

#[tokio::test]
async fn live_fetch_account_state_default_dex() {
    let c = client();
    let addr = test_address();
    let snap = c
        .fetch_account_state(&addr, None)
        .await
        .expect("fetch state");
    assert_eq!(snap.address, addr);
    assert!(snap.account_value >= rust_decimal::Decimal::ZERO);
    eprintln!(
        "live state: positions={}, accountValue={}, withdrawable={}",
        snap.positions.len(),
        snap.account_value,
        snap.withdrawable
    );
}

#[tokio::test]
async fn live_fetch_open_orders_default_dex() {
    let c = client();
    let addr = test_address();
    let orders = c
        .fetch_open_orders(&addr, None)
        .await
        .expect("fetch orders");
    eprintln!("live open orders count (default dex): {}", orders.len());
}

#[tokio::test]
async fn live_fetch_meta_returns_btc_and_eth() {
    let c = client();
    let m = c.fetch_meta(None).await.expect("fetch meta");
    let btc = m.universe.iter().find(|u| u.name == "BTC").expect("BTC in meta");
    let eth = m.universe.iter().find(|u| u.name == "ETH").expect("ETH in meta");
    assert!(btc.max_leverage >= 10);
    assert!(eth.max_leverage >= 10);
}

#[tokio::test]
async fn live_fetch_book_snapshot_eth_has_quotes() {
    let c = client();
    let book = c
        .fetch_book_snapshot(&Symbol::new("ETH"))
        .await
        .expect("fetch ETH book");
    assert!(book.best_bid().is_some(), "ETH bid present");
    assert!(book.best_ask().is_some(), "ETH ask present");
    assert!(book.best_ask().unwrap() > book.best_bid().unwrap(), "spread positive");
}
```

- [ ] **Step 9.3: Verify live tests are skipped by default**

Run: `cd executor && cargo test -p executor-hl 2>&1 | tail -10`
Expected: parser tests + unit tests pass; the `live_mainnet_readonly` test file compiles to zero tests (because `cfg(feature = "live")` excludes it).

- [ ] **Step 9.4: Verify the live tests can be opted into**

Run: `cd executor && HL_TEST_ADDRESS=0xfe3e32cd4443e395ec0400bf828a34309e517d2d cargo test -p executor-hl --features live live_ -- --nocapture 2>&1 | tail -40`
Expected: 5 live tests run and pass; eprintln shows e.g. `positions=1, accountValue=...`.

(If the user wants to skip live tests during planning, they can omit Step 9.4 — but it's the acceptance gate.)

- [ ] **Step 9.5: Verify no live test ran in default `cargo test`**

Run: `cd executor && cargo test -p executor-hl 2>&1 | grep -E 'test result|live_' | head -10`
Expected: result line shows the count without live tests; no `live_` test names.

- [ ] **Step 9.6: Commit**

```bash
git add executor/crates/executor-hl/Cargo.toml executor/crates/executor-hl/tests/live_mainnet_readonly.rs
git commit -m "test(executor-hl): live mainnet read-only integration test (feature-gated)"
```

---

## Task 10: CI guard — ensure live tests stay opt-in

**Files:**
- Modify: `scripts/check_ci_local.sh` (only if it currently runs `cargo test --features live` or similar)
- Modify: `.github/workflows/*.yml` (only if a workflow needs an explicit skip for `live`)

- [ ] **Step 10.1: Inspect current CI**

Run: `grep -rE 'cargo test|features' scripts/check_ci_local.sh .github/workflows/ 2>/dev/null | head -20`

- [ ] **Step 10.2: If CI uses `--all-features`, exclude `live`**

If any line is like `cargo test --all-features`, change to:
```bash
cargo test --workspace --features '' --no-default-features ...
```
…or more practically, add a comment + explicit `--features` enumeration. **If CI does NOT use `--all-features`, skip this step.**

- [ ] **Step 10.3: Run the local CI script**

Run: `bash scripts/check_ci_local.sh 2>&1 | tail -30`
Expected: green. If `live` ran, fix Step 10.2 and re-run.

- [ ] **Step 10.4: Commit if any change was made**

```bash
git status -s scripts/check_ci_local.sh .github/workflows/
# If anything modified:
git add scripts/check_ci_local.sh  # or workflow files
git commit -m "ci: keep executor-hl 'live' feature off by default"
```

---

## Task 11: Gemini review + PR

**Files:**
- (none)

- [ ] **Step 11.1: Generate a focused diff for Gemini**

Run:
```bash
git diff develop...HEAD -- executor/crates/executor-hl/ scripts/sanitize_hl_fixture.py > /tmp/pr-a-diff.patch
wc -l /tmp/pr-a-diff.patch
```

- [ ] **Step 11.2: Run Gemini deep review**

Run:
```bash
cat /tmp/pr-a-diff.patch | ~/.claude/hooks/gemini-review.sh deep
```
Expected: a structured review (Pro model, multi-section). Save the output to `/tmp/pr-a-gemini-review.md`.

- [ ] **Step 11.3: Address review comments**

For each MUST-FIX:
1. Make the change.
2. Re-run `cargo test -p executor-hl` and `cargo clippy -p executor-hl --all-targets -- -D warnings`.
3. Commit each fix as its own commit (`fix(executor-hl): <comment summary>`).

For SUGGESTION items, use judgment — apply if quick, defer to a follow-up issue if larger.

- [ ] **Step 11.4: Open the PR**

Run:
```bash
git push -u origin feat/pr-a-hl-readonly-parser
gh pr create --title "feat(executor-hl): PR-A — HL mainnet read-only parser" --body "$(cat <<'EOF'
## Summary
- Adds `wire` module: typed Rust structs that deserialize HL `/info` JSON responses (clearinghouseState, openOrders, frontendOpenOrders, l2Book, meta, userRole).
- Extends `AccountStateSnapshot` with `account_value`, `withdrawable`, `cross_maintenance_margin_used`, `server_time`, and `from_wire` mapping.
- Extends `HlClient` trait with `fetch_open_orders`, `fetch_meta`, `fetch_user_role`, plus `dex: Option<&str>` on `fetch_account_state` for HIP-3 builder dexs.
- Implements all parsers in `RealHlClient` (read-only, no signing changes).
- 12 new fixture-driven unit tests + 5 opt-in live mainnet tests behind `--features live`.

## Test plan
- [ ] `cd executor && cargo test --workspace` — green
- [ ] `cd executor && cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `bash scripts/check_ci_local.sh` — green
- [ ] (manual) `HL_TEST_ADDRESS=<master EOA> cargo test -p executor-hl --features live live_ -- --nocapture` — 5 live tests pass against mainnet

## Notes
- Live tests are SKIPPED by default in CI; opt in with `--features live`.
- No PK touched — PR-A is read-only only. Signing/place/cancel arrives in PR-B (Stage B).
- Fixtures sanitized via `scripts/sanitize_hl_fixture.py`; no real master/agent addresses in git history.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Print the PR URL.

- [ ] **Step 11.5: Final acceptance checks**

Verify against spec §5.6:
- [ ] `cargo test -p executor-hl --test parse_clearinghouse_state` — 4 tests pass (3 parser + 1 mapping)
- [ ] `cargo test -p executor-hl --test parse_open_orders` — 3 tests pass
- [ ] (live) `cargo test -p executor-hl --features live live_ -- --nocapture` — 5 tests pass; eprintln output shows assetPositions and openOrders counts matching today's mainnet snapshot
- [ ] `cargo fmt --check` — clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `bash scripts/check_ci_local.sh` — green

---

## Plan Self-Review

**Spec coverage check (§5.1–5.6):**

| Spec requirement | Plan task |
|---|---|
| §5.2 `clearinghouseState` (default dex) | Tasks 3, 7 |
| §5.2 `clearinghouseState` (HIP-3 dex) | Task 3 (xyz fixture), Task 8 (`dex` arg) |
| §5.2 `openOrders` (default + dex) | Task 4 (parsing), Task 8 (trait + RealHlClient) |
| §5.2 `l2Book` | Task 5 |
| §5.2 `meta` | Task 6 |
| §5.2 `userRole` | Task 6 + Task 8 (Role enum + trait) |
| §5.3 struct shapes | Tasks 3, 4, 5, 6, 7, 8 |
| §5.4 `serde-with-str` | Task 3 (every numeric field uses `rust_decimal::serde::str`) |
| §5.4 `userRole` startup check | Task 8 (`fetch_user_role` exposed); call from `executor-server` belongs to a follow-up since spec §5.6 only requires the parser |
| §5.4 multi-dex `Symbol` `xyz:META` | Already supported by existing `Symbol::is_hip3()`; verified by Task 3 xyz fixture |
| §5.4 rate limit `TokenBucket` reuse | Task 8 (`acquire(2)` for weight-2 endpoints, `acquire(20)` for others) |
| §5.5 unit tests | Tasks 3–7 |
| §5.5 live integration (feature-gated) | Task 9 |
| §5.5 CI default-off | Task 10 |
| §5.6 4 acceptance bullets | Task 11.5 |

No gaps.

**Type consistency:**
- `AccountStateSnapshot` field names (`account_value`, `withdrawable`, `cross_maintenance_margin_used`) appear identically in Tasks 3, 7, 9.
- `HlOpenOrder` (renamed from `OpenOrder` to avoid colliding with `executor_core::state::OpenOrder`) used consistently in Tasks 4, 8, 9.
- `Role` enum used consistently in Tasks 6, 8, 9.
- `dex: Option<&str>` parameter signature identical across trait and call sites in Task 8.
- `WireOrderSide` is `pub enum { A, B }` (no `serde(rename_all=UPPERCASE)` because variant names are already single uppercase letters matching the wire). This is consistent with Tasks 4 + 8.
- `WireMeta.universe[].sz_decimals: u32` matches `executor-core` already-used `u32` for size precision.

**Placeholder scan:** No "TBD" / "implement later" / "similar to Task N" steps. Every code-changing step shows the exact code.

**Edge case acknowledged:** Task 7 Step 7.4 walks through a possible `Default` derivation removal — included with the explicit grep command to find call sites and a concrete migration to `AccountStateSnapshot::empty(...)`.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-05-pr-a-readonly-parser-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**