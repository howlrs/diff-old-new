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
use crate::intent_checker::IntentChecker;

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
#[derive(Clone)]
pub struct BatchSender {
    tx: mpsc::Sender<Envelope>,
    /// `None` = gate disabled (mock mode / pre-PR-C2 path).
    /// `Some(...)` = consulted before enqueueing each `OrderOrCancel::Place`;
    /// `OrderOrCancel::Cancel` always bypasses the gate (emergency_stop must work).
    gate: Option<Arc<dyn IntentChecker>>,
}

impl std::fmt::Debug for BatchSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchSender")
            .field("gate", &self.gate.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Envelope {
    item: OrderOrCancel,
    /// Caller that wants per-item completion notification (optional).
    ack: Option<oneshot::Sender<Result<(), HlError>>>,
}

impl BatchSender {
    pub fn enqueue(&self, item: OrderOrCancel) -> Result<(), HlError> {
        if let Some(reason) = self.gate_reject_reason(&item) {
            return Err(HlError::ActionFormat(format!("safety_gate: {reason}")));
        }
        self.tx
            .try_send(Envelope { item, ack: None })
            .map_err(|e| HlError::Network(format!("batch enqueue: {e}")))
    }

    /// Enqueue and await the per-item ack from the flusher.
    pub async fn enqueue_with_ack(
        &self,
        item: OrderOrCancel,
    ) -> Result<oneshot::Receiver<Result<(), HlError>>, HlError> {
        if let Some(reason) = self.gate_reject_reason(&item) {
            return Err(HlError::ActionFormat(format!("safety_gate: {reason}")));
        }
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

    /// Gate-only check: returns `Some(reason)` when the place should be
    /// rejected, `None` when it passes (or is a Cancel / no gate configured).
    /// Logs the rejection at error level so operators can see safety drops
    /// without having to thread the BatchSender's tracing context to the call site.
    fn gate_reject_reason(&self, item: &OrderOrCancel) -> Option<String> {
        let gate = self.gate.as_ref()?;
        let intent = match item {
            OrderOrCancel::Place(o) => o,
            OrderOrCancel::Cancel(_) => return None,
        };
        match gate.check_place(intent) {
            Ok(()) => None,
            Err(reason) => {
                tracing::error!(
                    reason = %reason,
                    cloid = ?intent.cloid,
                    symbol = %intent.symbol,
                    "safety_gate: drop place"
                );
                Some(reason)
            }
        }
    }
}

/// Spawned flusher task handle; await on this to know when it has shut down.
#[must_use]
pub struct BatchSenderHandle {
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

/// Spawn a BatchSender + flusher pair without a safety gate. Equivalent to
/// `spawn_batch_sender_with_gate(client, None, cfg)`.
pub fn spawn_batch_sender<C>(
    client: Arc<C>,
    cfg: BatchSenderConfig,
) -> (BatchSender, BatchSenderHandle)
where
    C: HlClient + 'static,
{
    spawn_batch_sender_with_gate(client, None, cfg)
}

/// Spawn a BatchSender + flusher pair with an optional `IntentChecker` gate.
/// PR-C2 (Gemini deep, 2026-05-05): real-mode startup wires this up so every
/// `OrderOrCancel::Place` is checked at enqueue time. `OrderOrCancel::Cancel`
/// always bypasses the gate so `emergency_stop` keeps working.
pub fn spawn_batch_sender_with_gate<C>(
    client: Arc<C>,
    gate: Option<Arc<dyn IntentChecker>>,
    cfg: BatchSenderConfig,
) -> (BatchSender, BatchSenderHandle)
where
    C: HlClient + 'static,
{
    let (tx, rx) = mpsc::channel(1024);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join = tokio::spawn(flusher_loop(client, rx, cfg, shutdown_rx));
    (
        BatchSender { tx, gate },
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
    // Gemini PR-2 review 反映: order と cancel を別々の ack 群で管理し,
    // それぞれの batch 結果を ack に伝搬する (失敗時は Err).
    let mut orders: Vec<OrderIntent> = Vec::new();
    let mut order_acks: Vec<oneshot::Sender<Result<(), HlError>>> = Vec::new();
    let mut cancels: Vec<CancelIntent> = Vec::new();
    let mut cancel_acks: Vec<oneshot::Sender<Result<(), HlError>>> = Vec::new();
    for env in buffer.drain(..) {
        match env.item {
            OrderOrCancel::Place(o) => {
                orders.push(o);
                if let Some(a) = env.ack {
                    order_acks.push(a);
                }
            }
            OrderOrCancel::Cancel(c) => {
                cancels.push(c);
                if let Some(a) = env.ack {
                    cancel_acks.push(a);
                }
            }
        }
    }

    if !orders.is_empty() {
        let res = client.place_orders(&orders).await;
        match &res {
            Ok(_) => tracing::trace!("flusher: placed {} orders", orders.len()),
            Err(e) => tracing::error!("flusher: place_orders failed: {e}"),
        }
        let summary = res.map(|_| ()).map_err(format_batch_err);
        for ack in order_acks {
            let _ = ack.send(clone_unit_result(&summary));
        }
    }
    if !cancels.is_empty() {
        let res = client.cancel_orders(&cancels).await;
        match &res {
            Ok(_) => tracing::trace!("flusher: cancelled {} orders", cancels.len()),
            Err(e) => tracing::error!("flusher: cancel_orders failed: {e}"),
        }
        let summary = res.map(|_| ()).map_err(format_batch_err);
        for ack in cancel_acks {
            let _ = ack.send(clone_unit_result(&summary));
        }
    }
}

/// HlError は Clone を持たないため, ack 1 つ毎に再生成する.
fn clone_unit_result(r: &Result<(), HlError>) -> Result<(), HlError> {
    match r {
        Ok(()) => Ok(()),
        Err(e) => Err(HlError::Network(format!("{e}"))),
    }
}

fn format_batch_err(e: HlError) -> HlError {
    match e {
        HlError::RateLimited { wait_ms } => HlError::RateLimited { wait_ms },
        other => HlError::Network(format!("batch failed: {other}")),
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

    // ---- PR-C2: gate tests ----

    #[derive(Debug)]
    struct AllowAll;

    impl IntentChecker for AllowAll {
        fn check_place(&self, _: &OrderIntent) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RejectAll;

    impl IntentChecker for RejectAll {
        fn check_place(&self, _: &OrderIntent) -> Result<(), String> {
            Err("blocked by test".into())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn enqueue_with_gate_rejects_violation() {
        let mock = Arc::new(MockHlClient::new());
        let (sender, handle) = spawn_batch_sender_with_gate(
            mock.clone(),
            Some(Arc::new(RejectAll) as Arc<dyn IntentChecker>),
            BatchSenderConfig::default(),
        );
        let res = sender.enqueue(OrderOrCancel::Place(order(Cloid::new())));
        let err = res.expect_err("gate must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("safety_gate") && msg.contains("blocked by test"),
            "unexpected error: {msg}"
        );
        // No flush should happen — buffer is empty.
        tokio::time::advance(Duration::from_millis(150)).await;
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        assert!(
            mock.placed_calls().is_empty(),
            "rejected orders must never reach client"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn enqueue_with_gate_passes_ok() {
        let mock = Arc::new(MockHlClient::new());
        let (sender, handle) = spawn_batch_sender_with_gate(
            mock.clone(),
            Some(Arc::new(AllowAll) as Arc<dyn IntentChecker>),
            BatchSenderConfig::default(),
        );
        sender
            .enqueue(OrderOrCancel::Place(order(Cloid::new())))
            .expect("AllowAll must pass");
        tokio::time::advance(Duration::from_millis(150)).await;
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        let calls = mock.placed_calls();
        assert_eq!(
            calls.iter().map(|v| v.len()).sum::<usize>(),
            1,
            "the single allowed order must reach client"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn enqueue_cancel_skips_gate() {
        let mock = Arc::new(MockHlClient::new());
        let (sender, handle) = spawn_batch_sender_with_gate(
            mock.clone(),
            Some(Arc::new(RejectAll) as Arc<dyn IntentChecker>),
            BatchSenderConfig::default(),
        );
        sender
            .enqueue(OrderOrCancel::Cancel(CancelIntent {
                symbol: Symbol::new("BTC"),
                by_cloid: Some(Cloid::new()),
                by_oid: None,
            }))
            .expect("Cancel must pass even with RejectAll gate");
        tokio::time::advance(Duration::from_millis(150)).await;
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        assert!(
            !mock.cancelled_calls().is_empty(),
            "cancel must reach client unchanged"
        );
    }
}
