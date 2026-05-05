//! REST handlers.
//!
//! - `GET /v1/health` — health snapshot
//! - `GET /v1/positions` — current positions
//! - `GET /v1/book/{symbol}` — top-of-book snapshot for one symbol
//! - `POST /v1/exec` — start an execution
//! - `GET /v1/exec/{id}` — execution status / final report
//! - `POST /v1/exec/{id}/cancel` — request abort

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use executor_algo::ExecutionContext;
use executor_core::intent::{AlgoParams, ExecutionId, ExecutionReport, Intent, Progress};
use executor_core::state::{HealthStatus, OrderBook, Position};
use executor_core::symbol::Symbol;

use crate::error::ServerError;
use crate::registry::{ExecutionHandle, ExecutionStatus};
use crate::router::{validate_algorithm_name, OrderRouter};
use crate::state::ServerState;

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub algorithms: Vec<&'static str>,
    pub health: HealthStatus,
    pub running_executions: usize,
}

pub async fn health(
    State(s): State<Arc<ServerState>>,
) -> Result<Json<HealthResponse>, ServerError> {
    let h = s.app_state.health.read().await.clone();
    let running = s.registry.list().await.len();
    Ok(Json(HealthResponse {
        status: "ok",
        algorithms: OrderRouter::supported(),
        health: h,
        running_executions: running,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionsResponse {
    pub positions: HashMap<String, Position>,
}

pub async fn positions(
    State(s): State<Arc<ServerState>>,
) -> Result<Json<PositionsResponse>, ServerError> {
    let g = s.app_state.position.read().await;
    let positions = g
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.clone()))
        .collect();
    Ok(Json(PositionsResponse { positions }))
}

pub async fn book(
    State(s): State<Arc<ServerState>>,
    Path(symbol): Path<String>,
) -> Result<Json<OrderBook>, ServerError> {
    let sym = Symbol::new(symbol);
    let g = s.app_state.book.read().await;
    g.get(&sym)
        .cloned()
        .map(Json)
        .ok_or_else(|| ServerError::NotFound(format!("book for {sym}")))
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartExecRequest {
    pub algorithm: String,
    pub symbol: String,
    pub intent: Intent,
    pub target_size: Decimal,
    #[serde(default)]
    pub params: AlgoParams,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartExecResponse {
    pub exec_id: ExecutionId,
    pub algorithm: String,
}

pub async fn start_exec(
    State(s): State<Arc<ServerState>>,
    Json(req): Json<StartExecRequest>,
) -> Result<Json<StartExecResponse>, ServerError> {
    validate_algorithm_name(&req.algorithm)?;

    // PR-C2 Layer 1 (REST entry): symbol allow-list + rough notional cap using
    // book.best_bid as the reference price. Strict per-order check happens at
    // BatchSender enqueue time (Layer 2).
    let symbol = Symbol::new(req.symbol.clone());
    let ref_px = {
        let book_g = s.app_state.book.read().await;
        book_g.get(&symbol).and_then(|b| b.best_bid())
    };
    s.safety
        .check_request(&symbol, req.target_size, ref_px)
        .map_err(|v| ServerError::BadRequest(format!("safety_gate: {v}")))?;

    let mut algo = OrderRouter::build(&req.algorithm)?;
    let exec_id = ExecutionId::new();

    let (abort_tx, abort_rx) = watch::channel(false);
    let (progress_tx, _) = broadcast::channel::<Progress>(256);
    // Bridge: per-algo mpsc → broadcast fan-out for WS subscribers.
    let (algo_progress_tx, mut algo_progress_rx) = tokio::sync::mpsc::channel::<Progress>(256);
    let bcast = progress_tx.clone();
    tokio::spawn(async move {
        while let Some(p) = algo_progress_rx.recv().await {
            // Best-effort fan-out — broadcast errors mean no subscribers.
            let _ = bcast.send(p);
        }
    });

    let ctx = ExecutionContext {
        exec_id,
        symbol,
        intent: req.intent,
        target_size: req.target_size,
        params: req.params,
        state: s.app_state.clone(),
        batch: s.batch_sender.clone(),
        progress: algo_progress_tx,
        abort: abort_rx,
    };

    let algorithm_name = algo.name().to_string();
    let join = tokio::spawn(async move { algo.run(ctx).await });
    let handle = ExecutionHandle::new(exec_id, algorithm_name.clone(), abort_tx, progress_tx, join);
    s.registry.insert(handle).await;

    Ok(Json(StartExecResponse {
        exec_id,
        algorithm: algorithm_name,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecStatusResponse {
    pub exec_id: ExecutionId,
    pub algorithm: String,
    pub status: ExecutionStatus,
    pub report: Option<ExecutionReport>,
    pub error: Option<String>,
}

pub async fn get_exec(
    State(s): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<ExecStatusResponse>, ServerError> {
    let exec_id = parse_exec_id(&id)?;
    let entry = s
        .registry
        .get(&exec_id)
        .await
        .ok_or_else(|| ServerError::NotFound(id.clone()))?;
    let mut h = entry.write().await;
    // Poll the join handle without blocking — if the algo finished, capture
    // the result and update status.
    if h.status == ExecutionStatus::Running {
        // Gemini PR-7 review: avoid a race where one request takes the join
        // handle and another sees `join == None && status == Running`. Move
        // status to `Finalizing` *first* (still inside the write lock), then
        // take the handle and await it.
        let finished_handle = match h.join.as_ref() {
            Some(join) if join.is_finished() => {
                h.status = ExecutionStatus::Finalizing;
                h.join.take()
            }
            _ => None,
        };
        if let Some(handle) = finished_handle {
            match handle.await {
                Ok(Ok(report)) => {
                    h.status = if report.aborted {
                        ExecutionStatus::Aborted
                    } else {
                        ExecutionStatus::Completed
                    };
                    h.final_report = Some(report);
                }
                Ok(Err(e)) => {
                    h.status = ExecutionStatus::Failed;
                    h.final_error = Some(e.to_string());
                }
                Err(je) => {
                    h.status = ExecutionStatus::Failed;
                    h.final_error = Some(format!("join error: {je}"));
                }
            }
        }
    }
    Ok(Json(ExecStatusResponse {
        exec_id: h.exec_id,
        algorithm: h.algorithm.clone(),
        status: h.status,
        report: h.final_report.clone(),
        error: h.final_error.clone(),
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelExecResponse {
    pub exec_id: ExecutionId,
    pub abort_signaled: bool,
}

pub async fn cancel_exec(
    State(s): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<CancelExecResponse>, ServerError> {
    let exec_id = parse_exec_id(&id)?;
    let entry = s
        .registry
        .get(&exec_id)
        .await
        .ok_or_else(|| ServerError::NotFound(id.clone()))?;
    let h = entry.read().await;
    let _ = h.abort.send(true);
    Ok(Json(CancelExecResponse {
        exec_id,
        abort_signaled: true,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct EmergencyStopResponse {
    /// Number of running executions that received the abort signal.
    pub aborted_executions: usize,
    /// Number of resting orders cancelled across all symbols.
    pub cancelled_orders: usize,
}

/// `POST /v1/emergency_stop` — kill switch.
///
/// Gemini PR-8 review: order matters. Cancel resting orders FIRST so they
/// stop trading even if a still-running algo is about to enqueue more —
/// then abort the algos so they don't repost. Both signals are dispatched
/// before the function returns; the actual cancellation hits HL on the
/// next BatchSender flush (≤100 ms).
///
/// Operator auditability: the request may set `X-Operator-ID` so the log
/// line records who triggered the stop.
pub async fn emergency_stop(
    State(s): State<Arc<ServerState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<EmergencyStopResponse>, ServerError> {
    use executor_core::intent::CancelIntent;
    use executor_hl::batch_sender::OrderOrCancel;

    let operator = headers
        .get("x-operator-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Step 1: snapshot open orders + enqueue cancels (cancel before abort).
    let open_orders: Vec<_> = {
        let g = s.app_state.open_orders.read().await;
        g.values().cloned().collect()
    };
    let mut cancelled = 0usize;
    for order in open_orders {
        let intent = CancelIntent {
            symbol: order.symbol.clone(),
            by_cloid: Some(order.cloid),
            by_oid: order.oid,
        };
        if s.batch_sender
            .enqueue(OrderOrCancel::Cancel(intent))
            .is_ok()
        {
            cancelled += 1;
        }
    }

    // Step 2: abort all running executions (so they stop reposting).
    let aborted_executions = s.registry.abort_all().await;

    tracing::warn!(
        operator,
        aborted_executions,
        cancelled_orders = cancelled,
        "emergency_stop dispatched"
    );
    Ok(Json(EmergencyStopResponse {
        aborted_executions,
        cancelled_orders: cancelled,
    }))
}

fn parse_exec_id(s: &str) -> Result<ExecutionId, ServerError> {
    use std::str::FromStr;
    uuid::Uuid::from_str(s)
        .map(ExecutionId)
        .map_err(|e| ServerError::BadRequest(format!("invalid exec_id: {e}")))
}
