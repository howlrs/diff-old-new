//! `ServerState` — shared bag passed via axum's `with_state`.
//!
//! Wraps the `AppState` (book/position/...), the `BatchSender` used by all
//! algorithms, the `HlClient` (Mock or Real), the `Signer`, and the
//! `ExecutionRegistry` of running executions.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use executor_core::state::AppState;
use executor_hl::batch_sender::{BatchSender, BatchSenderHandle};
use executor_hl::hl_client::HlClient;
use executor_hl::signer::Signer;

use crate::registry::ExecutionRegistry;
use crate::safety::SafetyGate;

/// Server-side state. `Arc<ServerState>` is shared via `with_state`.
pub struct ServerState {
    pub app_state: Arc<AppState>,
    pub hl_client: Arc<dyn HlClient>,
    pub signer: Arc<dyn Signer>,
    pub batch_sender: BatchSender,
    pub batch_handle: tokio::sync::Mutex<Option<BatchSenderHandle>>,
    pub registry: ExecutionRegistry,
    pub safety: Arc<SafetyGate>,
    /// PR-C3: flipped to `true` once any caller initiates emergency_stop.
    /// Used to:
    /// - idempotency-gate `execute_emergency_stop` (only the first caller does the work)
    /// - reject new `start_exec` calls with HTTP 503 after a stop
    pub shutdown_initiated: AtomicBool,
}

impl ServerState {
    pub fn new(
        app_state: Arc<AppState>,
        hl_client: Arc<dyn HlClient>,
        signer: Arc<dyn Signer>,
        batch_sender: BatchSender,
        batch_handle: BatchSenderHandle,
        safety: Arc<SafetyGate>,
    ) -> Self {
        Self {
            app_state,
            hl_client,
            signer,
            batch_sender,
            batch_handle: tokio::sync::Mutex::new(Some(batch_handle)),
            registry: ExecutionRegistry::new(),
            safety,
            shutdown_initiated: AtomicBool::new(false),
        }
    }
}

impl std::fmt::Debug for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerState")
            .field("app_state", &self.app_state)
            .field("registry", &self.registry)
            .field("safety", &self.safety)
            .field(
                "shutdown_initiated",
                &self
                    .shutdown_initiated
                    .load(std::sync::atomic::Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}
