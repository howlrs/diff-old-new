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
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cloned_ref_to_slice_refs
)]

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

const MAX_NOTIONAL_USD: Decimal = dec!(5);
const ORDER_SZ_ETH: Decimal = dec!(0.001);
const PRICE_OFFSET_RATIO: Decimal = dec!(0.99);
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

fn make_client() -> RealHlClient {
    let signer = Arc::new(
        Eip712AgentSigner::from_secret(agent_pk_secret(), true /* is_mainnet */)
            .expect("Eip712AgentSigner::from_secret failed; HL_AGENT_PK malformed?"),
    );
    RealHlClient::new(HlConfig::mainnet(), signer)
}

#[tokio::test]
async fn live_mainnet_place_cancel_eth_round_trip() {
    let client = make_client();
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

    // === ETH index resolve via fetch_meta (PR-A path) ===
    let meta = client.fetch_meta(None).await.expect("fetch meta");
    let eth_idx = meta
        .universe
        .iter()
        .position(|u| u.name == "ETH")
        .expect("ETH not in default perp universe") as u32;
    eprintln!("ETH asset index: {}", eth_idx);

    // === best price ===
    let book = client
        .fetch_book_snapshot(&Symbol::new("ETH"))
        .await
        .expect("fetch ETH book");
    let best_bid = book.best_bid().expect("ETH bid present");
    let order_px = best_bid * PRICE_OFFSET_RATIO;
    let notional = order_px * ORDER_SZ_ETH;
    assert!(
        notional < MAX_NOTIONAL_USD,
        "size cap violated: notional ${} >= max ${}",
        notional,
        MAX_NOTIONAL_USD
    );
    eprintln!(
        "ETH best_bid={}, order_px={} (1% below), notional≈${}",
        best_bid, order_px, notional
    );

    // === place ALO post-only ===
    let cloid = Cloid::new();
    let intent = OrderIntent {
        cloid,
        symbol: Symbol::new("ETH"),
        asset: eth_idx,
        side: Side::Long,
        px: order_px,
        sz: ORDER_SZ_ETH,
        tif: Tif::Alo,
        reduce_only: false,
    };
    let place_resp = client
        .place_orders(&[intent.clone()])
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
    assert_eq!(pr.status, "resting", "expected resting, got {}", pr.status);
    let oid = pr.oid.expect("resting must have oid");

    // brief wait so HL has time to reflect the open order on the API
    tokio::time::sleep(Duration::from_millis(POST_PLACE_WAIT_MS)).await;

    // === cancel by cloid ===
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        asset: eth_idx,
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
