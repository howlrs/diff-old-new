//! executor-server: axum REST + WS bin (PR-7).
//!
//! 80 % prototype: uses `MockHlClient` + `MockSigner` so it runs end-to-end
//! without keys or networking. Real signer + real HL POST will be wired in a
//! follow-up after key-management brainstorming.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use executor_core::state::AppState;
use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
use executor_hl::hl_client::MockHlClient;
use executor_hl::signer::MockSigner;
use executor_server::{build_app, ServerState};

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

    let app_state = Arc::new(AppState::new());
    let mock_hl: Arc<MockHlClient> = Arc::new(MockHlClient::new());
    let signer = Arc::new(MockSigner::new());

    let (batch_sender, batch_handle) = spawn_batch_sender(
        mock_hl.clone(),
        BatchSenderConfig {
            flush_interval: Duration::from_millis(100),
            max_batch_size: 50,
        },
    );

    let state = Arc::new(ServerState::new(
        app_state,
        mock_hl,
        signer,
        batch_sender,
        batch_handle,
    ));

    let app = build_app(state);
    let bind = std::env::var("EXECUTOR_BIND").unwrap_or_else(|_| "0.0.0.0:8085".into());
    tracing::info!(%bind, "executor-server listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
