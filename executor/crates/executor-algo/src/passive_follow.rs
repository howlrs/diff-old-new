//! `PassiveFollowAlgorithm` — maker-style execution that joins the touch.
//!
//! Behavior (design v2 §5):
//! 1. Resolve `(side, abs_size)` from Intent + current position.
//! 2. Place an ALO (post-only) order at best-bid (long) or best-ask (short)
//!    sized at the remaining unfilled quantity.
//! 3. Watch the book at `repost_poll_ms` granularity. If the touch moves
//!    *away from* our price (best quote on our side improves), cancel our
//!    resting order and re-post at the new touch.
//! 4. Detect partial fills via `state.recent_fills` (cloid match) and
//!    decrement `remaining`. When the resting order is fully filled, remove
//!    it from open_orders and exit if `remaining == 0`.
//! 5. Stop when filled, aborted, total elapsed exceeds `max_total_ms`,
//!    or stale book.
//!
//! AlgoParams:
//! - `max_total_ms` (u32, default 60000) — total wall-clock budget
//! - `repost_poll_ms` (u32, default 250) — book poll interval
//! - `repost_threshold_ticks` (u32, default 0) — only repost if touch moved
//!   more than this many ticks (0 = repost on any change)
//! - `max_book_age_ms` (u32, default 500) — abort on stale WS
//! - `reduce_only` (bool, default false)
//!
//! The 80 % prototype path uses `MockHlClient` via the `BatchSender`, so unit
//! tests can run the full loop without keys or networking.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rust_decimal::Decimal;

use executor_core::cloid::Cloid;
use executor_core::errors::AlgoError;
#[cfg(test)]
use executor_core::intent::Intent;
use executor_core::intent::{AlgoParams, CancelIntent, ExecutionReport, OrderIntent, Progress};
use executor_core::state::{AppState, OrderBook};
use executor_core::symbol::Symbol;
use executor_core::types::{Side, Tif};

use executor_hl::batch_sender::OrderOrCancel;

use crate::algorithm::{build_report, drain_new_fills, Algorithm, ExecutionContext};
use crate::market::{ensure_book_fresh, resolve_side_and_size};

const DEFAULT_MAX_TOTAL_MS: u32 = 60_000;
const DEFAULT_REPOST_POLL_MS: u32 = 250;
const DEFAULT_REPOST_THRESHOLD_TICKS: u32 = 0;
const DEFAULT_MAX_BOOK_AGE_MS: u32 = 500;

#[derive(Debug, Clone)]
pub struct PassiveFollowParams {
    pub max_total: Duration,
    pub repost_poll: Duration,
    pub repost_threshold_ticks: u32,
    pub max_book_age: Option<Duration>,
    pub reduce_only: bool,
}

impl PassiveFollowParams {
    fn from_algo(p: &AlgoParams) -> Result<Self, AlgoError> {
        let max_total_ms = p.get_u32("max_total_ms").unwrap_or(DEFAULT_MAX_TOTAL_MS);
        if max_total_ms == 0 {
            return Err(AlgoError::InvalidParams("max_total_ms must be > 0".into()));
        }
        let repost_poll_ms = p
            .get_u32("repost_poll_ms")
            .unwrap_or(DEFAULT_REPOST_POLL_MS);
        if repost_poll_ms == 0 {
            return Err(AlgoError::InvalidParams(
                "repost_poll_ms must be > 0".into(),
            ));
        }
        let repost_threshold_ticks = p
            .get_u32("repost_threshold_ticks")
            .unwrap_or(DEFAULT_REPOST_THRESHOLD_TICKS);
        let max_book_age_ms = p
            .get_u32("max_book_age_ms")
            .unwrap_or(DEFAULT_MAX_BOOK_AGE_MS);
        let max_book_age = if max_book_age_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(max_book_age_ms as u64))
        };
        let reduce_only =
            p.0.get("reduce_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        Ok(Self {
            max_total: Duration::from_millis(max_total_ms as u64),
            repost_poll: Duration::from_millis(repost_poll_ms as u64),
            repost_threshold_ticks,
            max_book_age,
            reduce_only,
        })
    }
}

/// Pick our quote price from a book + side.
///
/// - Long: join best_bid (we sit at the bid).
/// - Short: join best_ask (we sit at the ask).
fn touch_for_side(book: &OrderBook, side: Side) -> Result<Decimal, AlgoError> {
    match side {
        Side::Long => book
            .best_bid()
            .ok_or_else(|| AlgoError::InvalidParams("passive: empty bids".into())),
        Side::Short => book
            .best_ask()
            .ok_or_else(|| AlgoError::InvalidParams("passive: empty asks".into())),
    }
}

#[derive(Debug, Default)]
pub struct PassiveFollowAlgorithm;

impl PassiveFollowAlgorithm {
    pub fn new() -> Self {
        Self
    }

    async fn snapshot_book(
        &self,
        state: &Arc<AppState>,
        symbol: &Symbol,
    ) -> Result<OrderBook, AlgoError> {
        let book = state.book.read().await;
        book.get(symbol)
            .cloned()
            .ok_or_else(|| AlgoError::InvalidParams(format!("no book for {symbol}")))
    }

    async fn snapshot_position_size(&self, state: &Arc<AppState>, symbol: &Symbol) -> Decimal {
        let pos = state.position.read().await;
        pos.get(symbol).map(|p| p.size).unwrap_or(Decimal::ZERO)
    }
}

#[async_trait::async_trait]
impl Algorithm for PassiveFollowAlgorithm {
    fn name(&self) -> &'static str {
        "PASSIVE_FOLLOW"
    }

    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError> {
        let started_at = Utc::now();
        let params = PassiveFollowParams::from_algo(&ctx.params)?;
        ctx.emit(Progress::Started {
            exec_id: ctx.exec_id,
            ts: started_at,
        })
        .await;

        let current = self.snapshot_position_size(&ctx.state, &ctx.symbol).await;
        let (side, abs_size) = resolve_side_and_size(ctx.intent, ctx.target_size, current)?;
        if abs_size <= Decimal::ZERO {
            return Err(AlgoError::InvalidParams("derived size <= 0".into()));
        }

        let mut remaining = abs_size;
        let mut all_fills = Vec::new();
        let mut own_cloids: HashSet<Cloid> = HashSet::new();
        let mut last_fill_idx = 0usize;
        let mut current_quote: Option<(Cloid, Decimal)> = None;
        let abort_rx = ctx.abort.clone();
        // tokio's Instant honors `tokio::time::pause()` in tests.
        let started_instant = tokio::time::Instant::now();

        // Outer loop: keep at most one resting order alive; repost on touch
        // movement, drain fills on every iteration.
        loop {
            if *abort_rx.borrow() {
                if let Some((c, _)) = current_quote.take() {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }
                return Ok(build_report(
                    ctx.exec_id,
                    self.name(),
                    started_at,
                    abs_size,
                    all_fills,
                    true,
                    Some("aborted by caller".into()),
                ));
            }
            if started_instant.elapsed() >= params.max_total {
                if let Some((c, _)) = current_quote.take() {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }
                return Ok(build_report(
                    ctx.exec_id,
                    self.name(),
                    started_at,
                    abs_size,
                    all_fills,
                    true,
                    Some(format!(
                        "max_total ({:?}) elapsed with {} remaining",
                        params.max_total, remaining
                    )),
                ));
            }

            // Drain any fills since last poll.
            let (new_fills, new_sz, idx) =
                drain_new_fills(&ctx.state, &own_cloids, last_fill_idx).await;
            last_fill_idx = idx;
            for f in &new_fills {
                ctx.emit(Progress::SliceFilled {
                    slice: 0,
                    cloid: f.cloid.unwrap_or_default(),
                    px: f.px,
                    sz: f.sz,
                    cumulative_filled: all_fills.iter().map(|x| x.sz).sum::<Decimal>() + f.sz,
                })
                .await;
            }
            all_fills.extend(new_fills);
            remaining = (remaining - new_sz).max(Decimal::ZERO);

            if remaining <= Decimal::ZERO {
                // Done. Cancel any leftover resting order (defensive).
                if let Some((c, _)) = current_quote.take() {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }
                break;
            }

            // Snapshot book and decide whether to repost.
            let book = self.snapshot_book(&ctx.state, &ctx.symbol).await?;
            ensure_book_fresh(&book, params.max_book_age)?;
            let new_touch = touch_for_side(&book, side)?;

            // 80 % プロト: tick size unknown until per-symbol meta wired.
            // Repost on any change of best touch. `repost_threshold_ticks` is
            // accepted for forward compatibility but currently a no-op.
            let need_repost = match &current_quote {
                None => true,
                Some((_, old_px)) => (new_touch - *old_px).abs() > Decimal::ZERO,
            };

            if need_repost {
                // Cancel the previous quote (if any), then enqueue a new ALO
                // at the current touch. Both go through the same BatchSender
                // so they coalesce in the next 100 ms flush.
                if let Some((c, _)) = current_quote.take() {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }
                let cloid = Cloid::new();
                own_cloids.insert(cloid);
                let order = OrderIntent {
                    cloid,
                    symbol: ctx.symbol.clone(),
                    side,
                    px: new_touch,
                    sz: remaining,
                    tif: Tif::Alo,
                    reduce_only: params.reduce_only,
                };
                tracing::debug!(
                    px = %order.px,
                    sz = %order.sz,
                    cloid = %cloid,
                    "passive_follow: repost ALO at touch"
                );
                ctx.batch
                    .enqueue(OrderOrCancel::Place(order))
                    .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;
                current_quote = Some((cloid, new_touch));
            }

            tokio::time::sleep(params.repost_poll).await;
        }

        let report = build_report(
            ctx.exec_id,
            self.name(),
            started_at,
            abs_size,
            all_fills,
            false,
            None,
        );
        ctx.emit(Progress::Completed {
            filled_size: report.filled_size,
            avg_px: report.avg_px.unwrap_or(Decimal::ZERO),
            total_fees: report.total_fees,
            n_fills: report.fills.len() as u32,
            ts: report.finished_at,
        })
        .await;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use executor_core::intent::ExecutionId;
    use executor_core::state::{BookLevel, Position};
    use executor_core::types::{Fill, OrderId};
    use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
    use executor_hl::hl_client::MockHlClient;
    use rust_decimal_macros::dec;
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};

    fn lvl(px: Decimal, sz: Decimal) -> BookLevel {
        BookLevel { px, sz, n: 1 }
    }

    fn algo_params_no_freshness() -> AlgoParams {
        let mut p = AlgoParams::default();
        p.0.insert("max_book_age_ms".into(), serde_json::json!(0));
        p
    }

    async fn set_book(state: &Arc<AppState>, symbol: &Symbol, ask: Decimal, bid: Decimal) {
        let mut b = state.book.write().await;
        b.insert(
            symbol.clone(),
            OrderBook {
                bids: vec![lvl(bid, dec!(10))],
                asks: vec![lvl(ask, dec!(10))],
                ts: Some(Utc::now()),
            },
        );
    }

    async fn set_position(state: &Arc<AppState>, symbol: &Symbol, size: Decimal) {
        let mut g = state.position.write().await;
        g.insert(
            symbol.clone(),
            Position {
                size,
                ..Default::default()
            },
        );
    }

    async fn push_fill(
        state: &Arc<AppState>,
        symbol: &Symbol,
        cloid: Cloid,
        side: Side,
        px: Decimal,
        sz: Decimal,
    ) {
        let mut f = state.recent_fills.write().await;
        let next_oid = f.len() as u64 + 1;
        f.push_back(Fill {
            symbol: symbol.clone(),
            cloid: Some(cloid),
            oid: OrderId(next_oid),
            side,
            px,
            sz,
            fee: Decimal::ZERO,
            ts: Utc::now(),
        });
    }

    #[test]
    fn touch_long_returns_best_bid() {
        let book = OrderBook {
            bids: vec![lvl(dec!(100), dec!(1))],
            asks: vec![lvl(dec!(101), dec!(1))],
            ts: None,
        };
        assert_eq!(touch_for_side(&book, Side::Long).unwrap(), dec!(100));
    }

    #[test]
    fn touch_short_returns_best_ask() {
        let book = OrderBook {
            bids: vec![lvl(dec!(100), dec!(1))],
            asks: vec![lvl(dec!(101), dec!(1))],
            ts: None,
        };
        assert_eq!(touch_for_side(&book, Side::Short).unwrap(), dec!(101));
    }

    #[test]
    fn touch_long_empty_bids_errors() {
        let book = OrderBook {
            bids: vec![],
            asks: vec![lvl(dec!(101), dec!(1))],
            ts: None,
        };
        assert!(touch_for_side(&book, Side::Long).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn passive_follow_places_alo_at_best_bid() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(50000)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Watcher: when the first ALO is placed, fill it fully.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                let mut all_orders: Vec<_> = Vec::new();
                for batch in &calls {
                    for o in batch {
                        all_orders.push(o.clone());
                    }
                }
                for o in all_orders {
                    if handled.contains(&o.cloid) {
                        continue;
                    }
                    handled.insert(o.cloid);
                    push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz).await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(2000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.5),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..200 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(0.5));

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        assert!(!placed.is_empty(), "expected at least one place call");
        let first = &placed[0];
        assert_eq!(first.tif, Tif::Alo, "must be post-only");
        assert_eq!(first.px, dec!(50000), "long → join best bid");
        assert_eq!(first.side, Side::Long);

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn passive_follow_reposts_when_touch_moves() {
        let symbol = Symbol::new("ETH");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(2001), dec!(2000)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Book mover: after some delay, raise the best bid so the algo must
        // cancel + repost. Then fill the new quote.
        let mover_state = state.clone();
        let mover_symbol = symbol.clone();
        let mover = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            set_book(&mover_state, &mover_symbol, dec!(2001), dec!(2000.5)).await;
        });

        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            // Only fill the SECOND placed cloid so the test exercises a repost.
            let mut count = 0u32;
            for _ in 0..500 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                let mut all_orders: Vec<_> = Vec::new();
                for batch in &calls {
                    for o in batch {
                        all_orders.push(o.clone());
                    }
                }
                for o in all_orders {
                    if handled.contains(&o.cloid) {
                        continue;
                    }
                    handled.insert(o.cloid);
                    count += 1;
                    if count >= 2 {
                        push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz)
                            .await;
                    }
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(5000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.5),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..500 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(0.5));

        // We expect at least 2 placed orders (initial + repost) + at least 1 cancel.
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let cancelled: Vec<_> = mock.cancelled_calls().into_iter().flatten().collect();
        assert!(
            placed.len() >= 2,
            "expected ≥2 placed (repost) got {}",
            placed.len()
        );
        assert!(
            !cancelled.is_empty(),
            "expected at least one cancel for the stale quote"
        );

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
        mover.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn passive_follow_close_short_buys_back_at_bid() {
        let symbol = Symbol::new("ETH");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(2001), dec!(2000)).await;
        set_position(&state, &symbol, dec!(-0.5)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                let mut all_orders: Vec<_> = Vec::new();
                for batch in &calls {
                    for o in batch {
                        all_orders.push(o.clone());
                    }
                }
                for o in all_orders {
                    if handled.contains(&o.cloid) {
                        continue;
                    }
                    handled.insert(o.cloid);
                    push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz).await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(2000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Close,
            target_size: Decimal::ZERO,
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..200 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted);
        assert_eq!(report.filled_size, dec!(0.5));
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let first = &placed[0];
        assert_eq!(first.side, Side::Long); // closing a short = buy back
                                            // Maker on the buy side joins the bid, not the ask.
        assert_eq!(first.px, dec!(2000), "long maker joins best bid");
        assert_eq!(first.tif, Tif::Alo);
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn passive_follow_aborts_immediately_when_signaled() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(101), dec!(100)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );
        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (abort_tx, abort_rx) = watch::channel(false);
        let _ = abort_tx.send(true);

        let mut algo = PassiveFollowAlgorithm::new();
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(1.0),
            params: algo_params_no_freshness(),
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };
        let report = algo.run(ctx).await.expect("ok");
        assert!(report.aborted);
        assert_eq!(report.filled_size, Decimal::ZERO);
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }

    /// Gemini PR-4 review: simulate "partial fill + book move" — first
    /// quote fills 30 % of size, then book moves so the algo cancels
    /// remaining and re-posts at the new touch with reduced size, which
    /// fills in full. Verifies that all_fills aggregates correctly across
    /// the partial + repost cycle.
    #[tokio::test(start_paused = true)]
    async fn passive_follow_partial_fill_then_book_move() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(50000)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Mover: bump the bid 200 ms in.
        let mover_state = state.clone();
        let mover_symbol = symbol.clone();
        let mover = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            set_book(&mover_state, &mover_symbol, dec!(50001), dec!(50000.5)).await;
        });

        // Watcher: fill 30 % on the first cloid; full fill on subsequent ones.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            let mut count = 0u32;
            for _ in 0..500 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                let mut all_orders: Vec<_> = Vec::new();
                for batch in &calls {
                    for o in batch {
                        all_orders.push(o.clone());
                    }
                }
                for o in all_orders {
                    if handled.contains(&o.cloid) {
                        continue;
                    }
                    handled.insert(o.cloid);
                    count += 1;
                    let fill_sz = if count == 1 { o.sz * dec!(0.3) } else { o.sz };
                    push_fill(
                        &watcher_state,
                        &watcher_symbol,
                        o.cloid,
                        o.side,
                        o.px,
                        fill_sz,
                    )
                    .await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(5000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(1.0),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..500 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(1.0), "should aggregate to full");
        assert!(report.fills.len() >= 2, "at least the partial + topup fill");

        // Sum of fill sizes must equal target.
        let total: Decimal = report.fills.iter().map(|f| f.sz).sum();
        assert_eq!(total, dec!(1.0));

        // The second order's size should equal remaining after partial = 0.7.
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        assert!(
            placed.len() >= 2,
            "expected ≥2 placed orders (initial + repost)"
        );
        // The repost order was sized at the remainder, not the original target.
        let second = &placed[1];
        assert_eq!(second.sz, dec!(0.7), "repost should size to remainder");

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
        mover.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn passive_follow_times_out_with_max_total() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(101), dec!(100)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );
        // No watcher → orders never fill.
        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("max_total_ms".into(), serde_json::json!(150));
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(1.0),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..30 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(report.aborted);
        assert!(report
            .abort_reason
            .as_ref()
            .map(|r| r.contains("max_total"))
            .unwrap_or(false));
        // The algo should have placed AND cancelled at least once on timeout.
        let cancelled: Vec<_> = mock.cancelled_calls().into_iter().flatten().collect();
        assert!(
            !cancelled.is_empty(),
            "timeout path must cancel resting order"
        );
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }
}
