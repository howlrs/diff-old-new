//! `TwapAlgorithm` — time-weighted average price execution.
//!
//! Behavior (design v2 §5):
//! 1. Resolve `(side, abs_size)` from Intent + current position.
//! 2. Divide `abs_size` evenly across `slice_count` slices.
//! 3. Every `interval_ms = total_duration_ms / slice_count`, emit a child
//!    "slice" using the configured child algorithm:
//!    - `market` (default): IoC slice with slippage cap (taker)
//!    - `passive`: post a single ALO at the touch and wait `interval_ms`
//! 4. Wait for the slice to fill (deadline = interval_ms - jitter), then
//!    move on to the next slice. Carry remainder forward.
//! 5. Stop when filled, aborted, total elapsed > `total_duration_ms`,
//!    or stale book.
//!
//! AlgoParams:
//! - `slice_count` (u32, default 10)
//! - `total_duration_ms` (u32, default 60000)
//! - `child_algo` ("market" | "passive", default "market")
//! - `max_slippage_bps` (forwarded to market child)
//! - `slice_timeout_ms` (forwarded to market child)
//! - `max_book_age_ms` (u32, default 500). `0` disables the freshness check
//!   — useful for unit tests with `tokio::time::pause()` where wall-clock
//!   `Utc::now()` keeps walking but virtual time does not.
//! - `reduce_only` (bool, default false)

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
use executor_core::types::{Fill, Side, Tif};

use executor_hl::batch_sender::OrderOrCancel;

use crate::algorithm::{
    build_report, drain_new_fills, taker_limit_price, Algorithm, ExecutionContext,
};
use crate::market::{ensure_book_fresh, resolve_side_and_size};

const DEFAULT_SLICE_COUNT: u32 = 10;
const DEFAULT_TOTAL_DURATION_MS: u32 = 60_000;
const DEFAULT_MAX_SLIPPAGE_BPS: &str = "20";
const DEFAULT_SLICE_TIMEOUT_MS: u32 = 1500;
const DEFAULT_MAX_BOOK_AGE_MS: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwapChild {
    /// Per-slice IoC order with slippage cap.
    Market,
    /// Per-slice ALO at the touch, waits for fills until next slice.
    Passive,
}

impl TwapChild {
    fn parse(s: &str) -> Result<Self, AlgoError> {
        match s.to_ascii_lowercase().as_str() {
            "market" => Ok(Self::Market),
            "passive" | "passive_follow" => Ok(Self::Passive),
            other => Err(AlgoError::InvalidParams(format!(
                "child_algo must be one of 'market' / 'passive', got '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TwapParams {
    pub slice_count: u32,
    pub total_duration: Duration,
    pub child: TwapChild,
    pub max_slippage_bps: Decimal,
    pub slice_timeout: Duration,
    pub max_book_age: Option<Duration>,
    pub reduce_only: bool,
}

impl TwapParams {
    fn from_algo(p: &AlgoParams) -> Result<Self, AlgoError> {
        let slice_count = p.get_u32("slice_count").unwrap_or(DEFAULT_SLICE_COUNT);
        if slice_count == 0 {
            return Err(AlgoError::InvalidParams("slice_count must be > 0".into()));
        }
        let total_ms = p
            .get_u32("total_duration_ms")
            .unwrap_or(DEFAULT_TOTAL_DURATION_MS);
        if total_ms == 0 {
            return Err(AlgoError::InvalidParams(
                "total_duration_ms must be > 0".into(),
            ));
        }
        let child = match p.0.get("child_algo").and_then(|v| v.as_str()) {
            Some(s) => TwapChild::parse(s)?,
            None => TwapChild::Market,
        };
        let max_slippage_bps = p
            .get_decimal("max_slippage_bps")
            .unwrap_or_else(|| DEFAULT_MAX_SLIPPAGE_BPS.parse().unwrap_or(Decimal::ZERO));
        if max_slippage_bps < Decimal::ZERO {
            return Err(AlgoError::InvalidParams(
                "max_slippage_bps must be >= 0".into(),
            ));
        }
        let slice_timeout_ms = p
            .get_u32("slice_timeout_ms")
            .unwrap_or(DEFAULT_SLICE_TIMEOUT_MS);
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
            slice_count,
            total_duration: Duration::from_millis(total_ms as u64),
            child,
            max_slippage_bps,
            slice_timeout: Duration::from_millis(slice_timeout_ms as u64),
            max_book_age,
            reduce_only,
        })
    }
}

#[derive(Debug, Default)]
pub struct TwapAlgorithm;

impl TwapAlgorithm {
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
impl Algorithm for TwapAlgorithm {
    fn name(&self) -> &'static str {
        "TWAP"
    }

    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError> {
        let started_at = Utc::now();
        let params = TwapParams::from_algo(&ctx.params)?;
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

        // Even slice sizing; carry rounding remainder onto the last slice.
        let slice_size_base = abs_size / Decimal::from(params.slice_count);
        let interval = params.total_duration / params.slice_count;

        let abort_rx = ctx.abort.clone();
        let started_instant = tokio::time::Instant::now();

        let mut all_fills: Vec<Fill> = Vec::new();
        let mut own_cloids: HashSet<Cloid> = HashSet::new();
        let mut last_fill_idx = 0usize;
        let mut filled_so_far = Decimal::ZERO;
        let mut current_passive_quote: Option<Cloid> = None;

        for slice_idx in 1..=params.slice_count {
            if *abort_rx.borrow() {
                if let Some(c) = current_passive_quote.take() {
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
            if started_instant.elapsed() >= params.total_duration {
                if let Some(c) = current_passive_quote.take() {
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
                        "total_duration ({:?}) elapsed at slice {}",
                        params.total_duration, slice_idx
                    )),
                ));
            }

            let target_at_slice = slice_size_base * Decimal::from(slice_idx);
            // Last slice: round any remainder.
            let target_at_slice = if slice_idx == params.slice_count {
                abs_size
            } else {
                target_at_slice
            };
            let slice_target_remaining = (target_at_slice - filled_so_far).max(Decimal::ZERO);
            if slice_target_remaining <= Decimal::ZERO {
                tokio::time::sleep(interval).await;
                continue;
            }

            let book = self.snapshot_book(&ctx.state, &ctx.symbol).await?;
            ensure_book_fresh(&book, params.max_book_age)?;

            // Cancel previous passive quote (we are about to repost, possibly
            // with a different size).
            if let Some(c) = current_passive_quote.take() {
                let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                    symbol: ctx.symbol.clone(),
                    by_cloid: Some(c),
                    by_oid: None,
                }));
            }

            let cloid = Cloid::new();
            own_cloids.insert(cloid);
            match params.child {
                TwapChild::Market => {
                    let limit_px = taker_limit_price(&book, side, params.max_slippage_bps)?;
                    let order = OrderIntent {
                        cloid,
                        symbol: ctx.symbol.clone(),
                        side,
                        px: limit_px,
                        sz: slice_target_remaining,
                        tif: Tif::Ioc,
                        reduce_only: params.reduce_only,
                    };
                    tracing::debug!(
                        slice = slice_idx,
                        sz = %order.sz,
                        px = %order.px,
                        cloid = %cloid,
                        "twap: market slice"
                    );
                    ctx.batch
                        .enqueue(OrderOrCancel::Place(order))
                        .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;
                }
                TwapChild::Passive => {
                    let touch = match side {
                        Side::Long => book
                            .best_bid()
                            .ok_or_else(|| AlgoError::InvalidParams("twap: empty bids".into()))?,
                        Side::Short => book
                            .best_ask()
                            .ok_or_else(|| AlgoError::InvalidParams("twap: empty asks".into()))?,
                    };
                    let order = OrderIntent {
                        cloid,
                        symbol: ctx.symbol.clone(),
                        side,
                        px: touch,
                        sz: slice_target_remaining,
                        tif: Tif::Alo,
                        reduce_only: params.reduce_only,
                    };
                    tracing::debug!(
                        slice = slice_idx,
                        sz = %order.sz,
                        px = %order.px,
                        cloid = %cloid,
                        "twap: passive slice"
                    );
                    ctx.batch
                        .enqueue(OrderOrCancel::Place(order))
                        .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;
                    current_passive_quote = Some(cloid);
                }
            }

            // Wait for slice fills. Use slice_timeout for market (IoC; should
            // come back fast) or interval for passive (sit until the next slice).
            let wait = match params.child {
                TwapChild::Market => params.slice_timeout.min(interval),
                TwapChild::Passive => interval,
            };
            let wait_until = tokio::time::Instant::now() + wait;
            let poll = Duration::from_millis(50);
            while tokio::time::Instant::now() < wait_until {
                if *abort_rx.borrow() {
                    break;
                }
                let (new_fills, new_sz, idx) =
                    drain_new_fills(&ctx.state, &own_cloids, last_fill_idx).await;
                last_fill_idx = idx;
                for f in &new_fills {
                    ctx.emit(Progress::SliceFilled {
                        slice: slice_idx,
                        cloid: f.cloid.unwrap_or_default(),
                        px: f.px,
                        sz: f.sz,
                        cumulative_filled: filled_so_far + new_sz,
                    })
                    .await;
                }
                filled_so_far += new_sz;
                all_fills.extend(new_fills);
                if filled_so_far >= target_at_slice {
                    break;
                }
                tokio::time::sleep(poll).await;
            }

            // Heartbeat between slices.
            ctx.emit(Progress::Heartbeat {
                cumulative_filled: filled_so_far,
                remaining: (abs_size - filled_so_far).max(Decimal::ZERO),
                ts: Utc::now(),
            })
            .await;
        }

        // Final cancel of any leftover passive quote.
        if let Some(c) = current_passive_quote.take() {
            let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                symbol: ctx.symbol.clone(),
                by_cloid: Some(c),
                by_oid: None,
            }));
        }

        // Final drain (catches any straggler fills).
        let (final_fills, final_sz, _) =
            drain_new_fills(&ctx.state, &own_cloids, last_fill_idx).await;
        filled_so_far += final_sz;
        all_fills.extend(final_fills);

        let aborted = filled_so_far < abs_size;
        let abort_reason = if aborted {
            Some(format!(
                "twap finished with {filled_so_far} of {abs_size} filled"
            ))
        } else {
            None
        };
        let report = build_report(
            ctx.exec_id,
            self.name(),
            started_at,
            abs_size,
            all_fills,
            aborted,
            abort_reason,
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
    use executor_core::types::OrderId;
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
                bids: vec![lvl(bid, dec!(100))],
                asks: vec![lvl(ask, dec!(100))],
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
    fn parse_child_algo() {
        assert_eq!(TwapChild::parse("market").unwrap(), TwapChild::Market);
        assert_eq!(TwapChild::parse("Market").unwrap(), TwapChild::Market);
        assert_eq!(TwapChild::parse("passive").unwrap(), TwapChild::Passive);
        assert_eq!(
            TwapChild::parse("passive_follow").unwrap(),
            TwapChild::Passive
        );
        assert!(TwapChild::parse("vwap").is_err());
    }

    #[test]
    fn from_algo_validates_zero_slice_count() {
        let mut p = AlgoParams::default();
        p.0.insert("slice_count".into(), serde_json::json!(0));
        assert!(TwapParams::from_algo(&p).is_err());
    }

    #[test]
    fn from_algo_validates_zero_duration() {
        let mut p = AlgoParams::default();
        p.0.insert("total_duration_ms".into(), serde_json::json!(0));
        assert!(TwapParams::from_algo(&p).is_err());
    }

    #[test]
    fn from_algo_negative_slippage_rejected() {
        let mut p = AlgoParams::default();
        p.0.insert("max_slippage_bps".into(), serde_json::json!("-1"));
        assert!(TwapParams::from_algo(&p).is_err());
    }

    /// Drives all 5 slices to fill via mock fills. Verifies aggregate
    /// filled_size = target and 5 distinct cloids submitted.
    #[tokio::test(start_paused = true)]
    async fn twap_market_emits_n_slices_to_fill_target() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50000), dec!(49999)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Watcher: fill every order in full.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
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
                    push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz).await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = TwapAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_count".into(), serde_json::json!(5));
        p.0.insert("total_duration_ms".into(), serde_json::json!(1000));
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(150));
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
        for _ in 0..200 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(1.0));

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let unique_cloids: HashSet<_> = placed.iter().map(|o| o.cloid).collect();
        assert_eq!(
            unique_cloids.len(),
            5,
            "expected 5 distinct slices, got {}",
            unique_cloids.len()
        );
        // Each slice should be sized at 0.2.
        for o in &placed {
            assert_eq!(o.sz, dec!(0.2));
            assert_eq!(o.tif, Tif::Ioc);
        }
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    /// child_algo = passive should issue ALO (post-only) orders.
    #[tokio::test(start_paused = true)]
    async fn twap_passive_uses_alo_at_touch() {
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

        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
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
                    push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz).await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = TwapAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_count".into(), serde_json::json!(3));
        p.0.insert("total_duration_ms".into(), serde_json::json!(900));
        p.0.insert("child_algo".into(), serde_json::json!("passive"));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.6),
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
        assert_eq!(report.filled_size, dec!(0.6));

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        for o in &placed {
            assert_eq!(o.tif, Tif::Alo, "passive child must use ALO");
            assert_eq!(o.px, dec!(50000), "long passive joins best bid");
        }
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    /// Close-short via TWAP: side flips to Long, slices are abs sized.
    #[tokio::test(start_paused = true)]
    async fn twap_close_short() {
        let symbol = Symbol::new("ETH");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(2001), dec!(2000)).await;
        set_position(&state, &symbol, dec!(-1.0)).await;

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
                    push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz).await;
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = TwapAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_count".into(), serde_json::json!(4));
        p.0.insert("total_duration_ms".into(), serde_json::json!(800));
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(150));
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
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(1.0));
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        for o in &placed {
            assert_eq!(o.side, Side::Long); // closing short = buy
            assert_eq!(o.sz, dec!(0.25)); // 1.0 / 4
        }
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn twap_aborts_immediately_when_signaled() {
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

        let mut algo = TwapAlgorithm::new();
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

    /// total_duration timeout returns a partial-filled report with abort_reason.
    #[tokio::test(start_paused = true)]
    async fn twap_total_duration_timeout_returns_partial() {
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

        let mut algo = TwapAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_count".into(), serde_json::json!(4));
        p.0.insert("total_duration_ms".into(), serde_json::json!(200));
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(40));
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
        for _ in 0..50 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(report.aborted);
        assert_eq!(report.filled_size, Decimal::ZERO);
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }
}
