//! PR-C4 placeholder: testnet multi-symbol cancel live test.
//!
//! Two HL testnet ALO post-only orders (ETH + BTC, ~$11 notional each) are
//! placed back-to-back; a single `cancel_orders(&[c1, c2])` then cancels both.
//! Existing positions and open orders on the master EOA must remain unchanged.
//!
//! Required env (testnet — separate from mainnet PK):
//!   HL_TESTNET_AGENT_PK — testnet agent wallet 64-hex private key
//!   HL_TESTNET_MASTER   — testnet master EOA (public hex address)
//!
//! Run only by the user from a shell where `source scripts/load-env-testnet.sh`
//! has been executed:
//!
//!     source scripts/load-env-testnet.sh
//!     cd executor
//!     cargo test -p executor-hl --features live \
//!       live_testnet_multi_cancel_two_symbols \
//!       -- --nocapture --test-threads=1
//!
//! Do NOT enable `--features live` in CI. The Claude PreToolUse hook
//! blocks `source scripts/load-env-testnet.sh`, so this test cannot fire
//! from the Claude session — only the user's interactive shell.

#![cfg(feature = "live")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use executor_core::cloid::Cloid;
use executor_core::intent::{CancelIntent, OrderIntent};
use executor_core::symbol::Symbol;
use executor_core::types::{Address, Side, Tif};
use executor_hl::hl_client::{HlClient, HlConfig, RealHlClient};
use executor_hl::signer::Eip712AgentSigner;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;

const TARGET_NOTIONAL_USD: Decimal = dec!(11);
const MAX_NOTIONAL_USD: Decimal = dec!(50);
const MIN_NOTIONAL_USD: Decimal = dec!(10);
const TICKS_BELOW_BID: usize = 100;
const POST_PLACE_WAIT_MS: u64 = 200;

fn master_address() -> Address {
    let s = std::env::var("HL_TESTNET_MASTER")
        .expect("HL_TESTNET_MASTER env required (testnet master EOA hex address)");
    Address::new(s)
}

fn agent_pk_secret() -> SecretString {
    let s = std::env::var("HL_TESTNET_AGENT_PK")
        .expect("HL_TESTNET_AGENT_PK env required (run `source scripts/load-env-testnet.sh`)");
    SecretString::new(s.into())
}

async fn make_client() -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(agent_pk_secret(), false /* is_mainnet */)
            .expect("Eip712AgentSigner::from_secret failed; HL_TESTNET_AGENT_PK malformed?"),
    );
    let bootstrap = RealHlClient::bootstrap(HlConfig::testnet(), signer);
    let meta = std::sync::Arc::new(
        executor_hl::meta::MetaCache::build(&bootstrap, &[None])
            .await
            .expect("MetaCache::build failed at PR-C4 testnet live test setup"),
    );
    bootstrap.with_meta(meta)
}

/// Compute a tick-grid-aligned safe-distance order price + a notional-targeted
/// size. Mirrors the pattern from `live_mainnet_place_cancel.rs` so prices
/// stay HL-valid by construction.
async fn build_place_intent(client: &RealHlClient, symbol: &Symbol, cloid: Cloid) -> OrderIntent {
    let book = client
        .fetch_book_snapshot(symbol)
        .await
        .unwrap_or_else(|e| panic!("fetch {symbol} book failed: {e}"));
    let best_bid = book
        .best_bid()
        .unwrap_or_else(|| panic!("{symbol} best_bid missing"));
    assert!(book.bids.len() >= 2, "{symbol}: need 2 bid levels");
    let tick = book.bids[0].px - book.bids[1].px;
    let order_px = best_bid - tick * Decimal::from(TICKS_BELOW_BID);
    let raw_sz = TARGET_NOTIONAL_USD / order_px;
    let order_sz = raw_sz.round_dp_with_strategy(4, rust_decimal::RoundingStrategy::ToZero);
    let notional = order_px * order_sz;
    assert!(
        notional >= MIN_NOTIONAL_USD && notional < MAX_NOTIONAL_USD,
        "{symbol}: notional ${} outside [{MIN_NOTIONAL_USD}, {MAX_NOTIONAL_USD})",
        notional
    );
    eprintln!(
        "{}: best_bid={} tick={} order_px={} order_sz={} notional≈${}",
        symbol, best_bid, tick, order_px, order_sz, notional
    );
    OrderIntent {
        cloid,
        symbol: symbol.clone(),
        side: Side::Long,
        px: order_px,
        sz: order_sz,
        tif: Tif::Alo,
        reduce_only: false,
    }
}

#[tokio::test]
async fn live_testnet_multi_cancel_two_symbols() {
    let client = make_client().await;
    let master = master_address();

    // === pre-snapshot ===
    let pre = client
        .fetch_account_state(&master, None)
        .await
        .expect("fetch testnet pre");
    eprintln!("PRE: positions={}", pre.positions.len());

    // === build 2 intents (ETH + BTC) ===
    let cloid_eth = Cloid::new();
    let cloid_btc = Cloid::new();
    let eth_intent = build_place_intent(&client, &Symbol::new("ETH"), cloid_eth).await;
    let btc_intent = build_place_intent(&client, &Symbol::new("BTC"), cloid_btc).await;

    // === place 2 in a single batch ===
    let place_resp = client
        .place_orders(&[eth_intent, btc_intent])
        .await
        .expect("place_orders multi network/sign error");
    assert_eq!(place_resp.len(), 2, "expected 2 responses");
    for (i, pr) in place_resp.iter().enumerate() {
        eprintln!(
            "PLACE[{}]: status={}, oid={:?}, error={:?}",
            i, pr.status, pr.oid, pr.error
        );
        if pr.status == "filled" {
            panic!(
                "UNEXPECTED FILL on slot {} — manual recovery required (oid={:?})",
                i, pr.oid
            );
        }
        assert_eq!(
            pr.status, "resting",
            "slot {}: expected resting, got {} — manual cleanup may be required",
            i, pr.status
        );
    }

    // brief settle
    tokio::time::sleep(Duration::from_millis(POST_PLACE_WAIT_MS)).await;

    // === single batch cancel (multi-symbol) ===
    let cancels = vec![
        CancelIntent {
            symbol: Symbol::new("ETH"),
            by_cloid: Some(cloid_eth),
            by_oid: None,
        },
        CancelIntent {
            symbol: Symbol::new("BTC"),
            by_cloid: Some(cloid_btc),
            by_oid: None,
        },
    ];
    let cancel_resp = client
        .cancel_orders(&cancels)
        .await
        .expect("cancel_orders multi network/sign error");
    assert_eq!(cancel_resp.len(), 2);
    for (i, cr) in cancel_resp.iter().enumerate() {
        eprintln!("CANCEL[{}]: status={}, error={:?}", i, cr.status, cr.error);
        assert_eq!(
            cr.status, "cancelled",
            "slot {}: expected cancelled, got {}",
            i, cr.status
        );
    }

    // === post-snapshot — existing positions unchanged ===
    let post = client
        .fetch_account_state(&master, None)
        .await
        .expect("fetch testnet post");
    eprintln!("POST: positions={}", post.positions.len());
    for (sym, pre_pos) in &pre.positions {
        let post_pos = post
            .positions
            .get(sym)
            .unwrap_or_else(|| panic!("symbol {sym} disappeared on testnet"));
        assert_eq!(
            pre_pos.size, post_pos.size,
            "{sym} szi changed during multi-symbol cancel test!"
        );
    }

    eprintln!(
        "✓ testnet multi-symbol cancel success: ETH cloid={} + BTC cloid={} both cancelled",
        cloid_eth, cloid_btc
    );
}
