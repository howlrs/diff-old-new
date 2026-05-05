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
