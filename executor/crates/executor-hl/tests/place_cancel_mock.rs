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
use executor_hl::meta::MetaCache;
use executor_hl::signer::Eip712AgentSigner;
use rust_decimal_macros::dec;
use secrecy::SecretString;
use std::sync::Arc;

const TEST_PK: &str = "0x0123456789012345678901234567890123456789012345678901234567890123";

/// Universe meta JSON used by `make_client` to populate the MetaCache.
/// Indices: BTC=0, ETH=1 (matching the original `asset: 1` literal for ETH).
const META_UNIVERSE_BODY: &str = r#"{"universe":[
    {"name":"BTC","szDecimals":5,"maxLeverage":40,"onlyIsolated":false},
    {"name":"ETH","szDecimals":4,"maxLeverage":25,"onlyIsolated":false}
]}"#;

/// Build a `RealHlClient` against the mock server, with a populated
/// MetaCache (BTC=0, ETH=1). Mocks the `/info meta` endpoint on the server
/// and consumes one mock; tests are responsible for any /exchange mocks.
async fn make_client(server: &mut mockito::ServerGuard) -> RealHlClient {
    let signer =
        Arc::new(Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap());
    let config = HlConfig {
        info_url: format!("{}/info", server.url()),
        exchange_url: format!("{}/exchange", server.url()),
        ws_url: "ws://unused".into(),
    };
    let _meta_mock = server
        .mock("POST", "/info")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(META_UNIVERSE_BODY)
        .expect_at_least(1)
        .create_async()
        .await;
    let bootstrap = RealHlClient::bootstrap(config, signer);
    let meta = Arc::new(
        MetaCache::build(&bootstrap, &[None])
            .await
            .expect("MetaCache::build failed against mocked /info"),
    );
    bootstrap.with_meta(meta)
}

fn make_order_intent() -> OrderIntent {
    OrderIntent {
        cloid: Cloid::new(),
        symbol: Symbol::new("ETH"),
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

    let client = make_client(&mut server).await;
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

    let client = make_client(&mut server).await;
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

    let client = make_client(&mut server).await;
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

    let client = make_client(&mut server).await;
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
    let mut server = mockito::Server::new_async().await;
    let client = make_client(&mut server).await;
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

    let client = make_client(&mut server).await;
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
async fn cancel_orders_by_oid_only_returns_action_format_error() {
    let mut server = mockito::Server::new_async().await;
    // by_oid-only error fires before resolve_asset, but make_client still
    // mocks /info to populate the MetaCache for consistency with other tests.
    let client = make_client(&mut server).await;
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        by_cloid: None,
        by_oid: Some(OrderId(99999)),
    };
    let err = client.cancel_orders(&[cancel]).await.unwrap_err();
    match err {
        HlError::ActionFormat(msg) => {
            assert!(
                msg.contains("by_oid-only cancel not supported"),
                "msg was: {msg}"
            );
        }
        other => panic!("expected ActionFormat err, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_orders_by_cloid_with_oid_present_uses_cloid_path() {
    // Gemini PR-B2a review fix: emergency_stop in routes.rs sets BOTH by_cloid
    // and by_oid. Earlier impl rejected the whole batch on `by_oid.is_some()`.
    // After fix, we silently use by_cloid and ignore by_oid for cancel-by-cloid path.
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

    let client = make_client(&mut server).await;
    let cloid = Cloid::new();
    let cancel = CancelIntent {
        symbol: Symbol::new("ETH"),
        by_cloid: Some(cloid),
        by_oid: Some(OrderId(99999)), // present alongside cloid; should be ignored
    };
    let resp = client.cancel_orders(&[cancel]).await.unwrap();
    assert_eq!(resp.len(), 1);
    assert_eq!(resp[0].status, "cancelled");
    assert_eq!(resp[0].cloid, cloid);
    assert!(resp[0].error.is_none());
}

#[tokio::test]
async fn place_orders_unknown_symbol_returns_error_response_no_http() {
    // RealHlClient::bootstrap() has an EMPTY MetaCache. Any symbol → UnknownSymbol.
    // Verify (a) the response is an error with "unknown symbol" message,
    // (b) no HTTP request is made (the mock server gets zero hits).
    let signer =
        Arc::new(Eip712AgentSigner::from_secret(SecretString::new(TEST_PK.into()), false).unwrap());
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
        resp[0].error.as_deref().unwrap().contains("unknown symbol"),
        "expected 'unknown symbol' in error: {:?}",
        resp[0].error
    );
}
