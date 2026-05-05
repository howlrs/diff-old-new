//! End-to-end integration tests against the axum app.
//!
//! Spins up the `Router` directly via `tower::ServiceExt::oneshot` so we
//! avoid binding a real socket. Uses MockHlClient + MockSigner.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use rust_decimal_macros::dec;
use serde_json::json;
use tower::ServiceExt;

use executor_core::state::{AppState, BookLevel, OrderBook, Position};
use executor_core::symbol::Symbol;
use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
use executor_hl::hl_client::MockHlClient;
use executor_hl::signer::MockSigner;
use executor_server::{build_app, SafetyGate, ServerState};

fn lvl(px: rust_decimal::Decimal, sz: rust_decimal::Decimal) -> BookLevel {
    BookLevel { px, sz, n: 1 }
}

async fn build_state_with_seed() -> (Arc<ServerState>, Arc<MockHlClient>) {
    build_state_with_safety(SafetyGate::disabled()).await
}

async fn build_state_with_safety(safety: SafetyGate) -> (Arc<ServerState>, Arc<MockHlClient>) {
    let app_state = Arc::new(AppState::new());
    {
        let mut b = app_state.book.write().await;
        b.insert(
            Symbol::new("BTC"),
            OrderBook {
                bids: vec![lvl(dec!(49999), dec!(10))],
                asks: vec![lvl(dec!(50001), dec!(10))],
                ts: Some(chrono::Utc::now()),
            },
        );
    }
    {
        let mut p = app_state.position.write().await;
        p.insert(
            Symbol::new("BTC"),
            Position {
                size: dec!(0),
                ..Default::default()
            },
        );
    }
    let mock_hl = Arc::new(MockHlClient::new());
    let signer = Arc::new(MockSigner::new());
    let (batch_sender, batch_handle) = spawn_batch_sender(
        mock_hl.clone(),
        BatchSenderConfig {
            flush_interval: Duration::from_millis(20),
            max_batch_size: 10,
        },
    );
    let state = Arc::new(ServerState::new(
        app_state,
        mock_hl.clone(),
        signer,
        batch_sender,
        batch_handle,
        Arc::new(safety),
    ));
    (state, mock_hl)
}

#[tokio::test]
async fn health_returns_ok() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert!(v["algorithms"].as_array().unwrap().len() >= 4);
}

#[tokio::test]
async fn book_returns_seeded_orderbook() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/book/BTC")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["bids"][0]["px"], "49999");
    assert_eq!(v["asks"][0]["px"], "50001");
}

#[tokio::test]
async fn book_missing_symbol_404() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/book/UNKNOWN")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn start_exec_unknown_algorithm_400() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let body = json!({
        "algorithm": "vwap",
        "symbol": "BTC",
        "intent": "open",
        "target_size": "0.1",
        "params": {}
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn start_exec_market_returns_id_and_can_be_cancelled() {
    let (state, _mock) = build_state_with_seed().await;
    let app = build_app(state);

    // Start a market execution. With no fills coming, it'll spin until
    // max_attempts → aborted. We immediately cancel to force exit.
    let req_body = json!({
        "algorithm": "market",
        "symbol": "BTC",
        "intent": "open",
        "target_size": "0.1",
        "params": {
            "max_book_age_ms": 0,
            "slice_timeout_ms": 50,
            "max_attempts": 1
        }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(req_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let exec_id = v["exec_id"].as_str().unwrap().to_string();
    assert_eq!(v["algorithm"], "MARKET");

    // Cancel it.
    let cancel_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/exec/{exec_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel_resp.status(), StatusCode::OK);

    // Give it a moment to settle, then poll status.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/exec/{exec_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);
    let body = to_bytes(status_resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let status = v["status"].as_str().unwrap();
    assert!(
        matches!(status, "aborted" | "completed" | "running" | "failed"),
        "unexpected status: {status}"
    );
}

#[tokio::test]
async fn cancel_unknown_exec_404() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec/00000000-0000-0000-0000-000000000000/cancel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_exec_invalid_id_400() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/exec/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn emergency_stop_aborts_running_and_cancels_open_orders() {
    let (state, _mock) = build_state_with_seed().await;

    // Pre-seed an open order so the cancel branch fires.
    {
        let mut g = state.app_state.open_orders.write().await;
        let cloid = executor_core::cloid::Cloid::new();
        g.insert(
            cloid,
            executor_core::state::OpenOrder {
                cloid,
                oid: None,
                symbol: Symbol::new("BTC"),
                side: executor_core::types::Side::Long,
                px: dec!(50000),
                sz: dec!(0.1),
                filled_sz: dec!(0),
                tif: executor_core::types::Tif::Alo,
                reduce_only: false,
                placed_at: chrono::Utc::now(),
            },
        );
    }

    let app = build_app(state.clone());

    // Start a running execution.
    let req_body = serde_json::json!({
        "algorithm": "passive",
        "symbol": "BTC",
        "intent": "open",
        "target_size": "0.5",
        "params": {
            "max_book_age_ms": 0,
            "repost_poll_ms": 100,
            "max_total_ms": 10000
        }
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(req_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Hit emergency stop.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/emergency_stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        v["aborted_executions"].as_u64().unwrap() >= 1,
        "expected at least 1 aborted execution"
    );
    assert!(
        v["cancelled_orders"].as_u64().unwrap() >= 1,
        "expected at least 1 cancelled order"
    );
}

#[tokio::test]
async fn emergency_stop_records_operator_header() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/emergency_stop")
                .header("x-operator-id", "alice@desk")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should still return 200 even with no running executions.
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["aborted_executions"], 0);
    assert_eq!(v["cancelled_orders"], 0);
}

#[tokio::test]
async fn positions_returns_seeded() {
    let (state, _) = build_state_with_seed().await;
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/positions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["positions"]["BTC"].is_object());
}

// ---- PR-C2: SafetyGate Layer 1 (REST entry) ----

#[tokio::test]
async fn start_exec_symbol_not_allowed_400() {
    // Allow-list = {BTC}; request comes in for ETH → 400.
    let safety = SafetyGate::from_args("BTC", None, false).unwrap();
    let (state, _) = build_state_with_safety(safety).await;
    let app = build_app(state);

    let req_body = json!({
        "algorithm": "market",
        "symbol": "ETH",
        "intent": "open",
        "target_size": "0.001",
        "params": {}
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(req_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let msg = v["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("safety_gate") && msg.contains("symbol_not_allowed"),
        "unexpected message: {msg}"
    );
}

#[tokio::test]
async fn start_exec_notional_exceeded_400() {
    // Allow-list = {BTC}; cap = $10. Seeded book best_bid = $49999.
    // target_size = 0.001 → notional ≈ $50 → 400.
    let safety = SafetyGate::from_args("BTC", Some(10), false).unwrap();
    let (state, _) = build_state_with_safety(safety).await;
    let app = build_app(state);

    let req_body = json!({
        "algorithm": "market",
        "symbol": "BTC",
        "intent": "open",
        "target_size": "0.001",
        "params": {}
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(req_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let msg = v["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("safety_gate") && msg.contains("notional_exceeded"),
        "unexpected message: {msg}"
    );
}

#[tokio::test]
async fn start_exec_within_caps_200() {
    // Allow-list = {BTC}; cap = $100. Seeded book best_bid = $49999.
    // target_size = 0.0001 → notional ≈ $5 → 200.
    let safety = SafetyGate::from_args("BTC", Some(100), false).unwrap();
    let (state, _) = build_state_with_safety(safety).await;
    let app = build_app(state);

    let req_body = json!({
        "algorithm": "market",
        "symbol": "BTC",
        "intent": "open",
        "target_size": "0.0001",
        "params": {
            "max_book_age_ms": 0,
            "slice_timeout_ms": 50,
            "max_attempts": 1
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/exec")
                .header("content-type", "application/json")
                .body(Body::from(req_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "within-cap request should pass"
    );
}
