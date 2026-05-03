//! executor-server: axum REST + WS server.
//!
//! Wires the algorithms (executor-algo) to a transport so external systems
//! (the Python research stack, ops dashboards, the CLI) can drive executions.
//!
//! Responsibilities:
//! - Hold the shared `AppState` (book / position / open_orders / fills / health)
//!   and the `BatchSender` flusher started at `serve()` time.
//! - Accept REST requests to start/cancel/inspect executions.
//! - Stream `Progress` events over WS for any execution id.
//! - Snapshot health, positions, and books for sanity checks.
//!
//! 80 % prototype: uses `MockHlClient` + `MockSigner` from executor-hl. Real
//! networking + signing is wired up in a later PR after key-management
//! brainstorming.

#![forbid(unsafe_code)]

pub mod error;
pub mod registry;
pub mod router;
pub mod routes;
pub mod state;
pub mod ws;

pub use error::ServerError;
pub use registry::{ExecutionHandle, ExecutionRegistry, ExecutionStatus};
pub use router::OrderRouter;
pub use state::ServerState;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

/// Build the axum application router. The returned `Router` is ready to
/// be served via `axum::serve`.
pub fn build_app(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/v1/health", get(routes::health))
        .route("/v1/positions", get(routes::positions))
        .route("/v1/book/{symbol}", get(routes::book))
        .route("/v1/exec", post(routes::start_exec))
        .route("/v1/exec/{id}", get(routes::get_exec))
        .route("/v1/exec/{id}/cancel", post(routes::cancel_exec))
        .route("/v1/exec/{id}/ws", get(ws::progress_ws))
        .route("/v1/emergency_stop", post(routes::emergency_stop))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
