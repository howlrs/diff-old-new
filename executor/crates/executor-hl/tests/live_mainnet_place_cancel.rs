//! Live mainnet 1-round-trip place + cancel test.
//!
//! Hits real HL mainnet `/exchange`. Requires:
//! - `HL_TEST_ADDRESS` env: master EOA (read-only baseline)
//! - `HL_AGENT_PK` env: agent wallet 64-hex private key (from pass-store)
//!
//! Run only by the user from a shell where `source scripts/load-env.sh`
//! has been executed:
//!
//!     source scripts/load-env.sh
//!     cd executor
//!     HL_TEST_ADDRESS=0xfe3e32cd4443e395ec0400bf828a34309e517d2d \
//!       cargo test -p executor-hl --features live \
//!       live_mainnet_place_cancel \
//!       -- --nocapture --test-threads=1
//!
//! Do NOT enable `--features live` in CI. The Claude PreToolUse hook
//! blocks `source scripts/load-env.sh`, so this test cannot fire from
//! the Claude session — only the user's interactive shell.

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

/// HL enforces a minimum order notional of $10. We target ~$15 by computing
/// size dynamically from the live ETH price, so the test stays correct
/// regardless of where ETH trades (the previous fixed `0.005` would Fail
/// outside the $2000-$4000 ETH range).
const TARGET_NOTIONAL_USD: Decimal = dec!(15);
const MAX_NOTIONAL_USD: Decimal = dec!(50);
const MIN_NOTIONAL_USD: Decimal = dec!(10);
/// Sanity bounds for the observed tick. ETH typical tick is $0.1; if the book
/// L1-L2 gap is wildly outside this range the book is too thin to use as a
/// tick proxy and we fail loudly instead of silently sending a malformed price.
const MIN_OBSERVED_TICK_USD: Decimal = dec!(0.05);
const MAX_OBSERVED_TICK_USD: Decimal = dec!(0.5);
/// Number of ticks below best_bid to place the ALO post-only order.
/// 100 ticks at typical $0.1 tick = ~$10 below (~0.4% safety distance).
const TICKS_BELOW_BID: usize = 100;
const POST_PLACE_WAIT_MS: u64 = 200;

fn master_address() -> Address {
    let s = std::env::var("HL_TEST_ADDRESS")
        .expect("HL_TEST_ADDRESS env required (master EOA, public hex address)");
    Address::new(s)
}

fn agent_pk_secret() -> SecretString {
    let s = std::env::var("HL_AGENT_PK")
        .expect("HL_AGENT_PK env required (run `source scripts/load-env.sh` first)");
    SecretString::new(s.into())
}

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

#[tokio::test]
async fn live_mainnet_place_cancel_eth_round_trip() {
    let client = make_client().await;
    let master = master_address();

    // === pre-snapshot ===
    let pre_default = client
        .fetch_account_state(&master, None)
        .await
        .expect("fetch default state pre");
    let pre_xyz = client
        .fetch_account_state(&master, Some("xyz"))
        .await
        .expect("fetch xyz state pre");
    let pre_xyz_orders = client
        .fetch_open_orders(&master, Some("xyz"))
        .await
        .expect("fetch xyz orders pre");

    eprintln!(
        "PRE: default positions={}, xyz positions={}, xyz orders={}",
        pre_default.positions.len(),
        pre_xyz.positions.len(),
        pre_xyz_orders.len()
    );

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

    // === best price + tick-based offset ===
    //
    // HL price formatting requires (a) max 5 significant figures, (b) max
    // (6 - szDecimals) decimal places. `best_bid * 0.99` typically produces
    // too many sig figs (e.g. 2384.7 * 0.99 = 2360.853). To stay HL-valid by
    // construction, we observe the tick from the bid grid (level[0].px -
    // level[1].px) and step `TICKS_BELOW_BID` ticks down — that price is
    // always on HL's grid. We also bound the observed tick: a thin book where
    // the L1-L2 gap is far from typical is rejected via assert.
    let book = client
        .fetch_book_snapshot(&Symbol::new("ETH"))
        .await
        .expect("fetch ETH book");
    let best_bid = book.best_bid().expect("ETH bid present");
    assert!(
        book.bids.len() >= 2,
        "need at least 2 bid levels to derive tick (got {} levels)",
        book.bids.len()
    );
    let tick = book.bids[0].px - book.bids[1].px;
    assert!(
        tick >= MIN_OBSERVED_TICK_USD && tick <= MAX_OBSERVED_TICK_USD,
        "observed tick ${} outside sanity range [${}, ${}] — book may be thin/abnormal: \
         bids[0]={} bids[1]={}",
        tick,
        MIN_OBSERVED_TICK_USD,
        MAX_OBSERVED_TICK_USD,
        book.bids[0].px,
        book.bids[1].px
    );
    let order_px = best_bid - tick * Decimal::from(TICKS_BELOW_BID);

    // Compute size dynamically from current price so notional ≈ TARGET regardless
    // of where ETH trades. ETH szDecimals=4 (verified via meta) so we round
    // to 4 decimals. truncating ToZero ensures we stay below the target rather
    // than overshooting.
    let raw_sz = TARGET_NOTIONAL_USD / order_px;
    let order_sz = raw_sz.round_dp_with_strategy(4, rust_decimal::RoundingStrategy::ToZero);
    let notional = order_px * order_sz;
    assert!(
        notional >= MIN_NOTIONAL_USD,
        "below HL min: notional ${} < min ${} (sz={}, px={})",
        notional,
        MIN_NOTIONAL_USD,
        order_sz,
        order_px
    );
    assert!(
        notional < MAX_NOTIONAL_USD,
        "size cap violated: notional ${} >= max ${}",
        notional,
        MAX_NOTIONAL_USD
    );
    let safety_pct = (best_bid - order_px) / best_bid * dec!(100);
    eprintln!(
        "ETH best_bid={}, tick={}, order_px={} ({} ticks below, ≈{:.2}% safety), \
         sz={} (target ${}), notional≈${}",
        best_bid,
        tick,
        order_px,
        TICKS_BELOW_BID,
        safety_pct,
        order_sz,
        TARGET_NOTIONAL_USD,
        notional
    );

    // === place ALO post-only ===
    let cloid = Cloid::new();
    let intent = OrderIntent {
        cloid,
        symbol: Symbol::new("ETH"),
        side: Side::Long,
        px: order_px,
        sz: order_sz,
        tif: Tif::Alo,
        reduce_only: false,
    };
    let place_resp = client
        .place_orders(std::slice::from_ref(&intent))
        .await
        .expect("place_orders network/sign error");
    assert_eq!(place_resp.len(), 1, "expected 1 response");
    let pr = &place_resp[0];
    eprintln!(
        "PLACE: status={}, oid={:?}, error={:?}",
        pr.status, pr.oid, pr.error
    );

    // r/s padding diagnostic — surface signature rejection clearly
    if pr.status == "error" {
        let msg = pr.error.as_deref().unwrap_or("");
        if msg.to_lowercase().contains("signature") {
            panic!("SIGNATURE REJECTED — likely r/s padding issue. msg={msg}");
        }
        panic!("place rejected with non-signature error: {msg}");
    }
    if pr.status == "filled" {
        panic!(
            "UNEXPECTED FILL — manual recovery required (oid={:?}, cloid={})",
            pr.oid, cloid
        );
    }
    assert_eq!(
        pr.status, "resting",
        "expected resting, got {} (cloid={}, oid={:?}, error={:?}) — \
         if oid is Some, an order may be open on HL UI requiring manual cancel",
        pr.status, cloid, pr.oid, pr.error
    );
    let oid = pr.oid.expect("resting must have oid");

    // brief wait so HL has time to reflect the open order on the API
    tokio::time::sleep(Duration::from_millis(POST_PLACE_WAIT_MS)).await;

    // === cancel by cloid ===
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        by_cloid: Some(cloid),
        by_oid: None,
    };
    let cancel_resp = client
        .cancel_orders(&[cancel])
        .await
        .expect("cancel_orders network/sign error");
    assert_eq!(cancel_resp.len(), 1);
    let cr = &cancel_resp[0];
    eprintln!("CANCEL: status={}, error={:?}", cr.status, cr.error);
    assert_eq!(
        cr.status, "cancelled",
        "expected cancelled, got {} (manual cleanup may be required, oid={})",
        cr.status, oid
    );

    // === post-snapshot ===
    let post_default = client
        .fetch_account_state(&master, None)
        .await
        .expect("fetch default state post");
    let post_xyz = client
        .fetch_account_state(&master, Some("xyz"))
        .await
        .expect("fetch xyz state post");
    let post_xyz_orders = client
        .fetch_open_orders(&master, Some("xyz"))
        .await
        .expect("fetch xyz orders post");

    eprintln!(
        "POST: default positions={}, xyz positions={}, xyz orders={}",
        post_default.positions.len(),
        post_xyz.positions.len(),
        post_xyz_orders.len()
    );

    // === diff guard: HYPE szi unchanged ===
    let hype_pre = pre_default.positions.get(&Symbol::new("HYPE"));
    let hype_post = post_default.positions.get(&Symbol::new("HYPE"));
    match (hype_pre, hype_post) {
        (Some(p1), Some(p2)) => assert_eq!(p1.size, p2.size, "HYPE szi changed!"),
        (None, None) => eprintln!("HYPE absent in both pre/post (ok if user closed it)"),
        _ => panic!("HYPE pre/post mismatch (one side absent)"),
    }

    // === diff guard: xyz:META szi unchanged ===
    let meta_pre = pre_xyz.positions.get(&Symbol::new("xyz:META"));
    let meta_post = post_xyz.positions.get(&Symbol::new("xyz:META"));
    match (meta_pre, meta_post) {
        (Some(p1), Some(p2)) => assert_eq!(p1.size, p2.size, "xyz:META szi changed!"),
        (None, None) => eprintln!("xyz:META absent in both pre/post"),
        _ => panic!("xyz:META pre/post mismatch (one side absent)"),
    }

    // === diff guard: xyz:GOOGL open order oids unchanged ===
    let mut googl_pre: Vec<u64> = pre_xyz_orders
        .iter()
        .filter(|o| o.symbol.as_str() == "xyz:GOOGL")
        .map(|o| o.oid.0)
        .collect();
    let mut googl_post: Vec<u64> = post_xyz_orders
        .iter()
        .filter(|o| o.symbol.as_str() == "xyz:GOOGL")
        .map(|o| o.oid.0)
        .collect();
    googl_pre.sort_unstable();
    googl_post.sort_unstable();
    assert_eq!(googl_pre, googl_post, "xyz:GOOGL open orders changed!");

    eprintln!(
        "✓ mainnet round trip success: place oid={} → cancel cloid={}",
        oid.0, cloid
    );
}
