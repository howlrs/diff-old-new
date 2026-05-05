//! `ServerState` — shared bag passed via axum's `with_state`.
//!
//! Wraps the `AppState` (book/position/...), the `BatchSender` used by all
//! algorithms, the `HlClient` (Mock or Real), the `Signer`, and the
//! `ExecutionRegistry` of running executions.

use std::sync::Arc;

use executor_core::state::AppState;
use executor_hl::batch_sender::{BatchSender, BatchSenderHandle};
use executor_hl::hl_client::HlClient;
use executor_hl::signer::Signer;

use crate::registry::ExecutionRegistry;

/// Server-side state. `Arc<ServerState>` is shared via `with_state`.
pub struct ServerState {
    pub app_state: Arc<AppState>,
    pub hl_client: Arc<dyn HlClient>,
    pub signer: Arc<dyn Signer>,
    pub batch_sender: BatchSender,
    pub batch_handle: tokio::sync::Mutex<Option<BatchSenderHandle>>,
    pub registry: ExecutionRegistry,
}

impl ServerState {
    pub fn new(
        app_state: Arc<AppState>,
        hl_client: Arc<dyn HlClient>,
        signer: Arc<dyn Signer>,
        batch_sender: BatchSender,
        batch_handle: BatchSenderHandle,
    ) -> Self {
        Self {
            app_state,
            hl_client,
            signer,
            batch_sender,
            batch_handle: tokio::sync::Mutex::new(Some(batch_handle)),
            registry: ExecutionRegistry::new(),
        }
    }
}

impl std::fmt::Debug for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerState")
            .field("app_state", &self.app_state)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}
