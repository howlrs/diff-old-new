//! `Algorithm` trait + `ExecutionContext` — the contract every execution
//! algorithm (MARKET, PASSIVE_FOLLOW, TWAP, MARKET_MAKE) implements.
//!
//! Design v2 §5 reflection:
//! - One `Algorithm::run` call corresponds to one execution. Multiple algo
//!   instances can run concurrently (each in its own tokio task).
//! - `ExecutionContext` bundles the shared state, the batch sender (for
//!   sending order/cancel intents), the progress channel (for streaming
//!   updates back to the server), and an `abort` watch (for emergency_stop).
//! - The algorithm does NOT touch the HL client directly; it only enqueues
//!   intents on the `BatchSender`. This guarantees rate-limit + 100 ms
//!   batching are uniformly applied.
//! - `recv_fills_until` lets an algo wait for its own fills (filtered by the
//!   set of cloids it sent). Fills are observed via the open-orders / fills
//!   slices of `AppState` populated by the WS layer.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rust_decimal::Decimal;
use tokio::sync::{mpsc, watch};

use executor_core::cloid::Cloid;
use executor_core::errors::AlgoError;
use executor_core::intent::{AlgoParams, ExecutionId, ExecutionReport, Intent, Progress};
use executor_core::state::AppState;
use executor_core::symbol::Symbol;
use executor_core::types::Fill;

use executor_hl::batch_sender::BatchSender;

/// A single command from an `Algorithm` to the executor server. Algorithms
/// publish progress events here; the server fans them out to subscribers.
pub type ProgressTx = mpsc::Sender<Progress>;

/// All inputs handed to one `Algorithm::run` invocation.
pub struct ExecutionContext {
    pub exec_id: ExecutionId,
    pub symbol: Symbol,
    pub intent: Intent,
    pub target_size: Decimal,
    pub params: AlgoParams,
    pub state: Arc<AppState>,
    pub batch: BatchSender,
    pub progress: ProgressTx,
    /// `true` once an external caller requests abort (emergency_stop, /cancel).
    pub abort: watch::Receiver<bool>,
}

impl ExecutionContext {
    /// Convenience: emit a `Progress` event, swallowing channel-closed errors
    /// (they only happen if every subscriber has hung up — algorithms should
    /// keep running in that case).
    pub async fn emit(&self, ev: Progress) {
        let _ = self.progress.send(ev).await;
    }

    /// Has the caller asked us to abort?
    pub fn is_aborting(&self) -> bool {
        *self.abort.borrow()
    }
}

/// The contract implemented by every execution algorithm.
///
/// Algorithms are spawned as `tokio::task::spawn(async move { algo.run(ctx).await })`.
/// `&mut self` lets impls hold per-run state (slice index, target snapshot,
/// etc) while still being object-safe through `Box<dyn Algorithm>`.
#[async_trait::async_trait]
pub trait Algorithm: Send + Sync {
    fn name(&self) -> &'static str;

    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError>;
}

/// Helper: build an `ExecutionReport` from a final fill set. Algorithms call
/// this at the end of `run` so the structure stays consistent across impls.
pub fn build_report(
    exec_id: ExecutionId,
    algorithm: &str,
    started_at: chrono::DateTime<Utc>,
    target_size: Decimal,
    fills: Vec<Fill>,
    aborted: bool,
    abort_reason: Option<String>,
) -> ExecutionReport {
    let filled_size: Decimal = fills.iter().map(|f| f.sz).sum();
    let total_fees: Decimal = fills.iter().map(|f| f.fee).sum();
    let avg_px = if filled_size > Decimal::ZERO {
        let weighted: Decimal = fills.iter().map(|f| f.px * f.sz).sum();
        Some(weighted / filled_size)
    } else {
        None
    };

    ExecutionReport {
        exec_id,
        algorithm: algorithm.to_string(),
        started_at,
        finished_at: Utc::now(),
        target_size,
        filled_size,
        avg_px,
        total_fees,
        fills,
        aborted,
        abort_reason,
    }
}

/// Drain `state.recent_fills` for the cloids we own, until either:
/// - the cumulative filled size reaches `target_size`, or
/// - the deadline elapses, or
/// - the abort signal is set.
///
/// Returns the set of fills that matched our cloids, plus a flag indicating
/// whether the wait completed (true = target reached) or timed out / aborted.
pub async fn collect_own_fills(
    state: &AppState,
    own_cloids: &HashSet<Cloid>,
    target_size: Decimal,
    poll: Duration,
    deadline: Duration,
    abort: &mut watch::Receiver<bool>,
) -> (Vec<Fill>, bool) {
    // Use tokio's `Instant` so deadlines respect `tokio::time::pause()`
    // / `tokio::time::advance()` in tests. `std::time::Instant` returns
    // wall-clock time, which would never elapse under paused virtual time.
    let started = tokio::time::Instant::now();
    let mut collected: Vec<Fill> = Vec::new();
    let mut filled_total = Decimal::ZERO;
    let mut last_seen_idx = 0usize;

    while filled_total < target_size {
        if *abort.borrow() {
            return (collected, false);
        }
        if started.elapsed() >= deadline {
            return (collected, false);
        }

        // Scan recent_fills for new entries with our cloids.
        {
            let fills_g = state.recent_fills.read().await;
            for (i, f) in fills_g.iter().enumerate().skip(last_seen_idx) {
                if let Some(c) = f.cloid {
                    if own_cloids.contains(&c) {
                        collected.push(f.clone());
                        filled_total += f.sz;
                    }
                }
                last_seen_idx = i + 1;
            }
        }

        if filled_total >= target_size {
            break;
        }
        tokio::time::sleep(poll).await;
    }
    (collected, true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use executor_core::types::{OrderId, Side};
    use rust_decimal_macros::dec;

    #[tokio::test(start_paused = true)]
    async fn collect_own_fills_returns_when_target_reached() {
        let state = Arc::new(AppState::new());
        let cloid = Cloid::new();
        let mut own = HashSet::new();
        own.insert(cloid);

        // Pre-seed two fills matching our cloid.
        {
            let mut fills = state.recent_fills.write().await;
            fills.push_back(Fill {
                symbol: Symbol::new("BTC"),
                cloid: Some(cloid),
                oid: OrderId(1),
                side: Side::Long,
                px: dec!(50000),
                sz: dec!(0.4),
                fee: dec!(0.1),
                ts: Utc::now(),
            });
            fills.push_back(Fill {
                symbol: Symbol::new("BTC"),
                cloid: Some(cloid),
                oid: OrderId(2),
                side: Side::Long,
                px: dec!(50001),
                sz: dec!(0.6),
                fee: dec!(0.1),
                ts: Utc::now(),
            });
        }
        let (_abort_tx, mut abort_rx) = watch::channel(false);
        let (got, ok) = collect_own_fills(
            &state,
            &own,
            dec!(1.0),
            Duration::from_millis(10),
            Duration::from_secs(5),
            &mut abort_rx,
        )
        .await;
        assert!(ok);
        assert_eq!(got.len(), 2);
        assert_eq!(got.iter().map(|f| f.sz).sum::<Decimal>(), dec!(1.0));
    }

    #[tokio::test(start_paused = true)]
    async fn collect_own_fills_returns_on_abort() {
        let state = Arc::new(AppState::new());
        let cloid = Cloid::new();
        let mut own = HashSet::new();
        own.insert(cloid);
        let (abort_tx, mut abort_rx) = watch::channel(false);
        let _ = abort_tx.send(true);
        let (got, ok) = collect_own_fills(
            &state,
            &own,
            dec!(1.0),
            Duration::from_millis(10),
            Duration::from_secs(5),
            &mut abort_rx,
        )
        .await;
        assert!(!ok);
        assert!(got.is_empty());
    }
}
