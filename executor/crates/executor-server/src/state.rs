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
use executor_hl::ws_subscriber::{WsStatus, WsSubscriberHandle};

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
    /// PR-D1: live observability snapshot for the WS subscriber. In mock mode
    /// this is `WsStatus::disabled()` (cheap, never updated).
    pub ws_status: Arc<WsStatus>,
    /// PR-D1: handle to the WS supervisor task. `None` in mock mode. The
    /// `Drop` impl on `WsSubscriberHandle` aborts the task so a process
    /// shutdown stops the supervisor.
    pub ws_handle: tokio::sync::Mutex<Option<WsSubscriberHandle>>,
}

impl ServerState {
    /// Construct a `ServerState`. Use `with_ws` afterwards to attach a live
    /// WS subscriber handle (real mode); mock mode leaves it `None`.
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
            ws_status: Arc::new(WsStatus::disabled()),
            ws_handle: tokio::sync::Mutex::new(None),
        }
    }

    /// PR-D1: install a WS subscriber. Replaces the disabled `ws_status` with
    /// the live one carried by the handle, and stows the handle for future
    /// shutdown. Call once at startup before sharing `Arc<Self>`.
    pub fn install_ws_subscriber(&mut self, handle: WsSubscriberHandle) {
        self.ws_status = handle.status.clone();
        // The handle is stowed in a Mutex so async shutdown paths can take it
        // out without a `&mut self`.
        if let Ok(mut g) = self.ws_handle.try_lock() {
            *g = Some(handle);
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
            .field(
                "ws_connected",
                &self
                    .ws_status
                    .connected
                    .load(std::sync::atomic::Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}
