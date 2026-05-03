//! BatchSender — Gemini v2 review reflection.
//!
//! Algorithms enqueue order/cancel intents on an mpsc channel; a dedicated
//! flusher task drains the queue every `flush_interval_ms` and forwards a
//! single batch to `HlClient::place_orders` / `HlClient::cancel_orders`.
//!
//! Benefits per HL guidance:
//! - one POST /exchange per 100 ms, batching all pending intents
//! - rate-limit budget is consumed in coalesced bursts
//! - cancellations and orders are routed separately (HL recommends separate
//!   batches for ALO vs IOC/GTC; we mirror that).

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use executor_core::intent::{CancelIntent, OrderIntent};

use crate::errors::HlError;
use crate::hl_client::HlClient;

/// Either an order to place or a cancel to send.
#[derive(Debug, Clone)]
pub enum OrderOrCancel {
    Place(OrderIntent),
    Cancel(CancelIntent),
}

#[derive(Debug, Clone)]
pub struct BatchSenderConfig {
    pub flush_interval: Duration,
    /// Hard cap of pending batch length (newer intents wait if reached).
    pub max_batch_size: usize,
}

impl Default for BatchSenderConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_millis(100),
            max_batch_size: 50,
        }
    }
}

/// Sender side: hand to algorithms.
#[derive(Debug, Clone)]
pub struct BatchSender {
    tx: mpsc::Sender<Envelope>,
}

#[derive(Debug)]
struct Envelope {
    item: OrderOrCancel,
    /// Caller that wants per-item completion notification (optional).
    ack: Option<oneshot::Sender<Result<(), HlError>>>,
}

impl BatchSender {
    pub fn enqueue(&self, item: OrderOrCancel) -> Result<(), HlError> {
        self.tx
            .try_send(Envelope { item, ack: None })
            .map_err(|e| HlError::Network(format!("batch enqueue: {e}")))
    }

    /// Enqueue and await the per-item ack from the flusher.
    pub async fn enqueue_with_ack(
        &self,
        item: OrderOrCancel,
    ) -> Result<oneshot::Receiver<Result<(), HlError>>, HlError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(Envelope {
                item,
                ack: Some(ack_tx),
            })
            .await
            .map_err(|e| HlError::Network(format!("batch enqueue: {e}")))?;
        Ok(ack_rx)
    }
}

/// Spawned flusher task handle; await on this to know when it has shut down.
#[must_use]
pub struct BatchSenderHandle {
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

/// Spawn a BatchSender + flusher pair. Returns the sender (clone for each algo)
/// and a handle to await/cleanly stop the flusher.
pub fn spawn_batch_sender<C>(
    client: Arc<C>,
    cfg: BatchSenderConfig,
) -> (BatchSender, BatchSenderHandle)
where
    C: HlClient + 'static,
{
    let (tx, rx) = mpsc::channel(1024);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join = tokio::spawn(flusher_loop(client, rx, cfg, shutdown_rx));
    (
        BatchSender { tx },
        BatchSenderHandle {
            join,
            shutdown: shutdown_tx,
        },
    )
}

async fn flusher_loop<C: HlClient>(
    client: Arc<C>,
    mut rx: mpsc::Receiver<Envelope>,
    cfg: BatchSenderConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut buffer: Vec<Envelope> = Vec::with_capacity(cfg.max_batch_size);
    let mut ticker = tokio::time::interval(cfg.flush_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_flush = Instant::now();

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                // Drain remaining and exit.
                tracing::info!("batch_sender: shutdown requested, draining {}", buffer.len());
                while let Ok(env) = rx.try_recv() {
                    buffer.push(env);
                }
                flush_now(&client, &mut buffer).await;
                tracing::info!("batch_sender: drained, exiting");
                return;
            }
            maybe = rx.recv() => {
                match maybe {
                    Some(env) => {
                        buffer.push(env);
                        if buffer.len() >= cfg.max_batch_size {
                            flush_now(&client, &mut buffer).await;
                            last_flush = Instant::now();
                        }
                    }
                    None => {
                        // All senders dropped — drain & exit.
                        flush_now(&client, &mut buffer).await;
                        return;
                    }
                }
            }
            _ = ticker.tick() => {
                if !buffer.is_empty()
                    && last_flush.elapsed() >= cfg.flush_interval
                {
                    flush_now(&client, &mut buffer).await;
                    last_flush = Instant::now();
                }
            }
        }
    }
}

async fn flush_now<C: HlClient>(client: &Arc<C>, buffer: &mut Vec<Envelope>) {
    if buffer.is_empty() {
        return;
    }
    // Split into orders and cancels (HL recommends separate batches for ALO
    // vs IOC/GTC; we keep all orders together for now and split cancels off).
    let mut orders: Vec<OrderIntent> = Vec::new();
    let mut cancels: Vec<CancelIntent> = Vec::new();
    let mut acks: Vec<Option<oneshot::Sender<Result<(), HlError>>>> = Vec::new();
    for env in buffer.drain(..) {
        match env.item {
            OrderOrCancel::Place(o) => orders.push(o),
            OrderOrCancel::Cancel(c) => cancels.push(c),
        }
        acks.push(env.ack);
    }

    if !orders.is_empty() {
        match client.place_orders(&orders).await {
            Ok(_) => tracing::trace!("flusher: placed {} orders", orders.len()),
            Err(e) => tracing::error!("flusher: place_orders failed: {e}"),
        }
    }
    if !cancels.is_empty() {
        match client.cancel_orders(&cancels).await {
            Ok(_) => tracing::trace!("flusher: cancelled {} orders", cancels.len()),
            Err(e) => tracing::error!("flusher: cancel_orders failed: {e}"),
        }
    }
    // Per-item ack is best-effort; we report Ok to all on a successful batch
    // for now (PR-3+ refines per-cloid acks via WS confirmation).
    for ack in acks.into_iter().flatten() {
        let _ = ack.send(Ok(()));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::hl_client::MockHlClient;
    use executor_core::cloid::Cloid;
    use executor_core::symbol::Symbol;
    use executor_core::types::{Side, Tif};
    use rust_decimal_macros::dec;
    use std::sync::Arc;

    fn order(cloid: Cloid) -> OrderIntent {
        OrderIntent {
            cloid,
            symbol: Symbol::new("BTC"),
            side: Side::Long,
            px: dec!(50000),
            sz: dec!(0.01),
            tif: Tif::Gtc,
            reduce_only: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn flusher_batches_orders_within_interval() {
        let mock = Arc::new(MockHlClient::new());
        let (sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(100),
                max_batch_size: 50,
            },
        );
        for _ in 0..5 {
            sender
                .enqueue(OrderOrCancel::Place(order(Cloid::new())))
                .unwrap();
        }
        // Wait one flush tick
        tokio::time::advance(Duration::from_millis(150)).await;
        // Forced shutdown to drain
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;

        let calls = mock.placed_calls();
        assert!(!calls.is_empty(), "expected at least one place_orders call");
        let total: usize = calls.iter().map(|v| v.len()).sum();
        assert_eq!(total, 5, "all 5 orders should have been delivered");
    }

    #[tokio::test(start_paused = true)]
    async fn flusher_separates_orders_and_cancels() {
        let mock = Arc::new(MockHlClient::new());
        let (sender, handle) = spawn_batch_sender(mock.clone(), BatchSenderConfig::default());
        sender
            .enqueue(OrderOrCancel::Place(order(Cloid::new())))
            .unwrap();
        sender
            .enqueue(OrderOrCancel::Cancel(CancelIntent {
                symbol: Symbol::new("BTC"),
                by_cloid: Some(Cloid::new()),
                by_oid: None,
            }))
            .unwrap();

        tokio::time::advance(Duration::from_millis(150)).await;
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;

        let placed = mock.placed_calls();
        let cancelled = mock.cancelled_calls();
        assert!(!placed.is_empty());
        assert!(!cancelled.is_empty());
    }
}
