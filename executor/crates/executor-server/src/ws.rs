//! WS handler streaming `Progress` events for one execution.
//!
//! Subscribers join the broadcast channel held by the `ExecutionHandle`. A
//! lagged receive (slow consumer) closes the WS — the client should
//! reconnect and replay if needed.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::extract::State;
use futures::StreamExt;
use tokio::sync::broadcast::error::RecvError;

use crate::error::ServerError;
use crate::state::ServerState;

pub async fn progress_ws(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, ServerError> {
    let exec_id = parse_exec_id(&id)?;
    let entry = state
        .registry
        .get(&exec_id)
        .await
        .ok_or_else(|| ServerError::NotFound(id.clone()))?;
    let progress = entry.read().await.progress.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        if let Err(e) = run_socket(socket, progress).await {
            tracing::warn!(?e, "ws closed with error");
        }
    }))
}

async fn run_socket(
    socket: WebSocket,
    progress: tokio::sync::broadcast::Sender<executor_core::intent::Progress>,
) -> anyhow::Result<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = progress.subscribe();
    let mut closed = false;
    while !closed {
        tokio::select! {
            evt = rx.recv() => {
                match evt {
                    Ok(p) => {
                        let payload = serde_json::to_string(&p)?;
                        if futures::SinkExt::send(&mut sender, Message::Text(payload.into()))
                            .await
                            .is_err()
                        {
                            closed = true;
                        }
                    }
                    Err(RecvError::Closed) => {
                        closed = true;
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "ws subscriber lagged, closing");
                        closed = true;
                    }
                }
            }
            // Drain client-side pings/pongs; if the client closes, we exit.
            client = receiver.next() => {
                match client {
                    Some(Ok(Message::Close(_))) | None => {
                        closed = true;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => closed = true,
                }
            }
        }
    }
    Ok(())
}

fn parse_exec_id(s: &str) -> Result<executor_core::intent::ExecutionId, ServerError> {
    use std::str::FromStr;
    uuid::Uuid::from_str(s)
        .map(executor_core::intent::ExecutionId)
        .map_err(|e| ServerError::BadRequest(format!("invalid exec_id: {e}")))
}
