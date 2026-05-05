//! Registry of running and completed executions.
//!
//! Holds one entry per `ExecutionId`:
//! - the `JoinHandle` of the algorithm task
//! - an `abort` `watch::Sender` to ask the algo to stop
//! - a `broadcast::Sender<Progress>` so multiple WS subscribers can stream
//!   the same execution
//! - the final `ExecutionReport` once the task finishes

use std::collections::HashMap;
use std::sync::Arc;

use executor_core::errors::AlgoError;
use executor_core::intent::{ExecutionId, ExecutionReport, Progress};
use tokio::sync::{broadcast, watch, RwLock};
use tokio::task::JoinHandle;

/// Per-execution status as observed by the server.
///
/// `Finalizing` covers the (very brief) window between the algo task ending
/// and the server having awaited the join handle to extract the final
/// `ExecutionReport`. It exists to give concurrent GETs an unambiguous
/// "almost done, come back in a moment" signal — Gemini PR-7 review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Running,
    Finalizing,
    Completed,
    Aborted,
    Failed,
}

#[derive(Debug)]
pub struct ExecutionHandle {
    pub exec_id: ExecutionId,
    pub algorithm: String,
    pub abort: watch::Sender<bool>,
    pub progress: broadcast::Sender<Progress>,
    pub join: Option<JoinHandle<Result<ExecutionReport, AlgoError>>>,
    pub status: ExecutionStatus,
    pub final_report: Option<ExecutionReport>,
    pub final_error: Option<String>,
}

impl ExecutionHandle {
    pub fn new(
        exec_id: ExecutionId,
        algorithm: String,
        abort: watch::Sender<bool>,
        progress: broadcast::Sender<Progress>,
        join: JoinHandle<Result<ExecutionReport, AlgoError>>,
    ) -> Self {
        Self {
            exec_id,
            algorithm,
            abort,
            progress,
            join: Some(join),
            status: ExecutionStatus::Running,
            final_report: None,
            final_error: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ExecutionRegistry {
    inner: RwLock<HashMap<ExecutionId, Arc<RwLock<ExecutionHandle>>>>,
}

impl ExecutionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, handle: ExecutionHandle) {
        let exec_id = handle.exec_id;
        let mut g = self.inner.write().await;
        g.insert(exec_id, Arc::new(RwLock::new(handle)));
    }

    pub async fn get(&self, exec_id: &ExecutionId) -> Option<Arc<RwLock<ExecutionHandle>>> {
        let g = self.inner.read().await;
        g.get(exec_id).cloned()
    }

    pub async fn list(&self) -> Vec<ExecutionId> {
        let g = self.inner.read().await;
        g.keys().copied().collect()
    }

    /// Signal abort to every running execution. Returns the count signaled.
    /// Used by `POST /v1/emergency_stop` (PR-8). Holds the registry read lock
    /// only briefly to collect the abort senders.
    pub async fn abort_all(&self) -> usize {
        let entries: Vec<_> = {
            let g = self.inner.read().await;
            g.values().cloned().collect()
        };
        let mut signaled = 0usize;
        for entry in entries {
            let h = entry.read().await;
            if h.status == ExecutionStatus::Running && h.abort.send(true).is_ok() {
                signaled += 1;
            }
        }
        signaled
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn register_and_lookup() {
        let reg = ExecutionRegistry::new();
        let id = ExecutionId::new();
        let (abort_tx, _abort_rx) = watch::channel(false);
        let (prog_tx, _) = broadcast::channel(16);
        let join = tokio::spawn(async {
            Err::<ExecutionReport, AlgoError>(AlgoError::Aborted("test".into()))
        });
        reg.insert(ExecutionHandle::new(
            id,
            "test".into(),
            abort_tx,
            prog_tx,
            join,
        ))
        .await;
        assert!(reg.get(&id).await.is_some());
        assert_eq!(reg.list().await.len(), 1);
    }

    #[tokio::test]
    async fn missing_id_returns_none() {
        let reg = ExecutionRegistry::new();
        let id = ExecutionId::new();
        assert!(reg.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn abort_all_signals_each_running_execution() {
        let reg = ExecutionRegistry::new();
        // Spin up two synthetic handles in Running state.
        for _ in 0..2 {
            let id = ExecutionId::new();
            let (abort_tx, mut abort_rx) = watch::channel(false);
            let (prog_tx, _) = broadcast::channel(8);
            let join = tokio::spawn(async move {
                while !*abort_rx.borrow() {
                    let _ = abort_rx.changed().await;
                }
                Ok::<_, AlgoError>(ExecutionReport {
                    exec_id: id,
                    algorithm: "test".into(),
                    started_at: chrono::Utc::now(),
                    finished_at: chrono::Utc::now(),
                    target_size: rust_decimal::Decimal::ZERO,
                    filled_size: rust_decimal::Decimal::ZERO,
                    avg_px: None,
                    total_fees: rust_decimal::Decimal::ZERO,
                    fills: vec![],
                    aborted: true,
                    abort_reason: Some("test".into()),
                })
            });
            reg.insert(ExecutionHandle::new(
                id,
                "test".into(),
                abort_tx,
                prog_tx,
                join,
            ))
            .await;
        }
        let signaled = reg.abort_all().await;
        assert_eq!(signaled, 2);
    }
}
