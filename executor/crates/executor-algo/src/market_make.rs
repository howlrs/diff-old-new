//! `MarketMakeAlgorithm` — target-inventory two-sided ALO market making.
//!
//! Behavior (design v2 §5):
//! 1. Read `target_size` as the desired position (signed). The algorithm
//!    quotes both sides to keep current position drifting toward target.
//! 2. At every iteration, quote:
//!      - bid_px = mid * (1 - spread_bps_each_side / 10000)
//!      - ask_px = mid * (1 + spread_bps_each_side / 10000)
//!
//!    Both sides ALO at `quote_size` (or smaller if approaching target).
//! 3. Skew: when `(target - current)` is positive (we want to be longer),
//!    bid size is full and ask size shrinks proportionally.
//! 4. Cancel and re-post when:
//!      - The mid moves more than `repost_bps_threshold`
//!      - Our quote on a side gets fully filled (other side may need resize)
//!      - Inventory updates from external trades change the skew
//! 5. Exit when:
//!      - Position reaches target ± `target_tolerance_size`
//!      - Total elapsed > `max_total_ms`
//!      - Aborted by caller
//!      - Stale book
//!
//! AlgoParams:
//! - `quote_size` (string→Decimal, required) — base quote size per side
//! - `spread_bps_each_side` (string→Decimal, default "10")
//! - `repost_bps_threshold` (string→Decimal, default "2") — repost on mid
//!   movement exceeding this many bps
//! - `max_total_ms` (u32, default 300000)
//! - `repost_poll_ms` (u32, default 250)
//! - `max_book_age_ms` (u32, default 500)
//! - `target_tolerance_size` (string→Decimal, default "0") — stop when
//!   abs(target - current) ≤ this
//!
//! Intent semantics: MARKET_MAKE uses `Intent::SetTarget` exclusively. The
//! `target_size` is the signed final desired position (positive = long).
//!
//! Known limitations (acceptable for the 80 % prototype, revisit later):
//! - `all_fills` grows unbounded for long-running market making. Operators
//!   should rotate executions every ~hour or persist to disk if running
//!   indefinitely.
//! - `BatchSender::enqueue` errors are treated as fatal (channel closed
//!   means the flusher is dead — no recovery is possible). The algo aborts
//!   with `AlgoError::HyperliquidError` rather than retrying.

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

use crate::algorithm::{
    build_report, drain_new_fills, ensure_book_fresh, Algorithm, ExecutionContext,
};

const DEFAULT_SPREAD_BPS_EACH_SIDE: &str = "10";
const DEFAULT_REPOST_BPS_THRESHOLD: &str = "2";
const DEFAULT_MAX_TOTAL_MS: u32 = 300_000;
const DEFAULT_REPOST_POLL_MS: u32 = 250;
const DEFAULT_MAX_BOOK_AGE_MS: u32 = 500;
const DEFAULT_TARGET_TOLERANCE_SIZE: &str = "0";

#[derive(Debug, Clone)]
pub struct MarketMakeParams {
    pub quote_size: Decimal,
    pub spread_bps_each_side: Decimal,
    pub repost_bps_threshold: Decimal,
    pub max_total: Duration,
    pub repost_poll: Duration,
    pub max_book_age: Option<Duration>,
    pub target_tolerance_size: Decimal,
}

impl MarketMakeParams {
    fn from_algo(p: &AlgoParams) -> Result<Self, AlgoError> {
        let quote_size = p
            .get_decimal("quote_size")
            .ok_or_else(|| AlgoError::InvalidParams("quote_size is required".into()))?;
        if quote_size <= Decimal::ZERO {
            return Err(AlgoError::InvalidParams("quote_size must be > 0".into()));
        }
        let spread_bps_each_side = p.get_decimal("spread_bps_each_side").unwrap_or_else(|| {
            DEFAULT_SPREAD_BPS_EACH_SIDE
                .parse()
                .unwrap_or(Decimal::ZERO)
        });
        if spread_bps_each_side < Decimal::ZERO {
            return Err(AlgoError::InvalidParams(
                "spread_bps_each_side must be >= 0".into(),
            ));
        }
        let repost_bps_threshold = p.get_decimal("repost_bps_threshold").unwrap_or_else(|| {
            DEFAULT_REPOST_BPS_THRESHOLD
                .parse()
                .unwrap_or(Decimal::ZERO)
        });
        if repost_bps_threshold < Decimal::ZERO {
            return Err(AlgoError::InvalidParams(
                "repost_bps_threshold must be >= 0".into(),
            ));
        }
        // Gemini PR-6 review: setting `repost_bps_threshold = 0` causes
        // a cancel/repost on every tick. Combined with a tight `repost_poll`,
        // that easily exhausts HL's rate-limit budget and burns nonces. Warn
        // the operator instead of erroring so emergency tests still work.
        if repost_bps_threshold == Decimal::ZERO {
            tracing::warn!(
                "market_make: repost_bps_threshold = 0 triggers reposting on \
                 every mid change — this can saturate the rate limiter. Set \
                 ≥1 bps for production."
            );
        }
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
        let max_book_age_ms = p
            .get_u32("max_book_age_ms")
            .unwrap_or(DEFAULT_MAX_BOOK_AGE_MS);
        let max_book_age = if max_book_age_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(max_book_age_ms as u64))
        };
        let target_tolerance_size = p.get_decimal("target_tolerance_size").unwrap_or_else(|| {
            DEFAULT_TARGET_TOLERANCE_SIZE
                .parse()
                .unwrap_or(Decimal::ZERO)
        });
        Ok(Self {
            quote_size,
            spread_bps_each_side,
            repost_bps_threshold,
            max_total: Duration::from_millis(max_total_ms as u64),
            repost_poll: Duration::from_millis(repost_poll_ms as u64),
            max_book_age,
            target_tolerance_size,
        })
    }
}

/// Internal state: one resting quote per side.
#[derive(Debug, Clone, Default)]
struct Quote {
    cloid: Option<Cloid>,
    px: Option<Decimal>,
    sz: Option<Decimal>,
}

/// Compute (bid_px, ask_px) from mid + spread.
fn quote_prices(mid: Decimal, spread_bps_each_side: Decimal) -> (Decimal, Decimal) {
    let factor = spread_bps_each_side / Decimal::from(10_000);
    let bid_px = mid - mid * factor;
    let ask_px = mid + mid * factor;
    (bid_px, ask_px)
}

/// Compute (bid_sz, ask_sz) given:
/// - position delta to target (positive = need to go longer)
/// - quote_size (max per side)
///
/// Linear skew: when delta is large positive (need long), bid_sz = full,
/// ask_sz = max(0, quote_size + delta). Symmetrically for negative delta.
/// Sizes are clamped to [0, quote_size * 2].
fn quote_sizes(delta_to_target: Decimal, quote_size: Decimal) -> (Decimal, Decimal) {
    let cap_each = quote_size + quote_size; // upper cap per side
    if delta_to_target == Decimal::ZERO {
        return (quote_size, quote_size);
    }
    if delta_to_target > Decimal::ZERO {
        // Need to be longer → bid bigger, ask smaller.
        let bid = (quote_size + delta_to_target).min(cap_each);
        let ask = (quote_size - delta_to_target).max(Decimal::ZERO);
        (bid, ask)
    } else {
        let abs_delta = -delta_to_target;
        let bid = (quote_size - abs_delta).max(Decimal::ZERO);
        let ask = (quote_size + abs_delta).min(cap_each);
        (bid, ask)
    }
}

#[derive(Debug, Default)]
pub struct MarketMakeAlgorithm;

impl MarketMakeAlgorithm {
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

    /// Decide whether the current `quote` needs replacement given the new
    /// (px, sz). Returns true if the price moved more than threshold or the
    /// size differs.
    fn needs_repost(
        existing: &Quote,
        new_px: Decimal,
        new_sz: Decimal,
        repost_bps_threshold: Decimal,
    ) -> bool {
        match (existing.cloid, existing.px, existing.sz) {
            (None, _, _) => new_sz > Decimal::ZERO,
            (Some(_), Some(old_px), Some(old_sz)) => {
                // Cancel if size dropped to zero (the side wants to retreat).
                if new_sz <= Decimal::ZERO {
                    return true;
                }
                // Repost on size change.
                if old_sz != new_sz {
                    return true;
                }
                if old_px > Decimal::ZERO {
                    let bps_moved = (new_px - old_px).abs() / old_px * Decimal::from(10_000);
                    bps_moved > repost_bps_threshold
                } else {
                    new_px != old_px
                }
            }
            _ => true,
        }
    }
}

#[async_trait::async_trait]
impl Algorithm for MarketMakeAlgorithm {
    fn name(&self) -> &'static str {
        "MARKET_MAKE"
    }

    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError> {
        let started_at = Utc::now();
        let params = MarketMakeParams::from_algo(&ctx.params)?;
        ctx.emit(Progress::Started {
            exec_id: ctx.exec_id,
            ts: started_at,
        })
        .await;

        let target = ctx.target_size;
        let abort_rx = ctx.abort.clone();
        let started_instant = tokio::time::Instant::now();

        let mut all_fills = Vec::new();
        let mut own_cloids: HashSet<Cloid> = HashSet::new();
        let mut last_fill_idx = 0usize;
        let mut bid_quote = Quote::default();
        let mut ask_quote = Quote::default();

        // Cancel all live quotes (used on exit / stale state).
        let cancel_quote = |ctx: &ExecutionContext, q: &mut Quote| {
            if let Some(c) = q.cloid.take() {
                let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                    symbol: ctx.symbol.clone(),
                    // TODO(PR-B2b): resolve via meta cache (currently placeholder)
                    asset: 0,
                    by_cloid: Some(c),
                    by_oid: None,
                }));
            }
            q.px = None;
            q.sz = None;
        };

        loop {
            if *abort_rx.borrow() {
                cancel_quote(&ctx, &mut bid_quote);
                cancel_quote(&ctx, &mut ask_quote);
                return Ok(build_report(
                    ctx.exec_id,
                    self.name(),
                    started_at,
                    target,
                    all_fills,
                    true,
                    Some("aborted by caller".into()),
                ));
            }
            if started_instant.elapsed() >= params.max_total {
                cancel_quote(&ctx, &mut bid_quote);
                cancel_quote(&ctx, &mut ask_quote);
                let current = self.snapshot_position_size(&ctx.state, &ctx.symbol).await;
                let aborted = (target - current).abs() > params.target_tolerance_size;
                return Ok(build_report(
                    ctx.exec_id,
                    self.name(),
                    started_at,
                    target,
                    all_fills,
                    aborted,
                    if aborted {
                        Some(format!("max_total elapsed at position {current}"))
                    } else {
                        None
                    },
                ));
            }

            // Drain fills from previous iteration.
            let (new_fills, _new_sz, idx) =
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
            // If a side's resting quote was filled, drop our local handle —
            // we'll re-post on the next iteration once skew is recomputed.
            for f in &new_fills {
                if let Some(c) = f.cloid {
                    if Some(c) == bid_quote.cloid {
                        bid_quote = Quote::default();
                    } else if Some(c) == ask_quote.cloid {
                        ask_quote = Quote::default();
                    }
                }
            }
            all_fills.extend(new_fills);

            // Check exit condition (position within tolerance of target).
            let current = self.snapshot_position_size(&ctx.state, &ctx.symbol).await;
            let delta = target - current;
            if delta.abs() <= params.target_tolerance_size {
                cancel_quote(&ctx, &mut bid_quote);
                cancel_quote(&ctx, &mut ask_quote);
                break;
            }

            // Snapshot book and compute the desired quotes.
            let book = self.snapshot_book(&ctx.state, &ctx.symbol).await?;
            ensure_book_fresh(&book, params.max_book_age)?;
            let mid = book.mid().ok_or_else(|| {
                AlgoError::InvalidParams("market_make: empty book (no mid)".into())
            })?;
            let (bid_px, ask_px) = quote_prices(mid, params.spread_bps_each_side);
            let (desired_bid_sz, desired_ask_sz) = quote_sizes(delta, params.quote_size);

            // Reconcile bid side.
            if Self::needs_repost(
                &bid_quote,
                bid_px,
                desired_bid_sz,
                params.repost_bps_threshold,
            ) {
                cancel_quote(&ctx, &mut bid_quote);
                if desired_bid_sz > Decimal::ZERO {
                    let cloid = Cloid::new();
                    own_cloids.insert(cloid);
                    let order = OrderIntent {
                        cloid,
                        symbol: ctx.symbol.clone(),
                        // TODO(PR-B2b): resolve via meta cache (currently placeholder)
                        asset: 0,
                        side: Side::Long,
                        px: bid_px,
                        sz: desired_bid_sz,
                        tif: Tif::Alo,
                        reduce_only: false,
                    };
                    ctx.batch
                        .enqueue(OrderOrCancel::Place(order))
                        .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;
                    bid_quote = Quote {
                        cloid: Some(cloid),
                        px: Some(bid_px),
                        sz: Some(desired_bid_sz),
                    };
                }
            }
            // Reconcile ask side.
            if Self::needs_repost(
                &ask_quote,
                ask_px,
                desired_ask_sz,
                params.repost_bps_threshold,
            ) {
                cancel_quote(&ctx, &mut ask_quote);
                if desired_ask_sz > Decimal::ZERO {
                    let cloid = Cloid::new();
                    own_cloids.insert(cloid);
                    let order = OrderIntent {
                        cloid,
                        symbol: ctx.symbol.clone(),
                        // TODO(PR-B2b): resolve via meta cache (currently placeholder)
                        asset: 0,
                        side: Side::Short,
                        px: ask_px,
                        sz: desired_ask_sz,
                        tif: Tif::Alo,
                        reduce_only: false,
                    };
                    ctx.batch
                        .enqueue(OrderOrCancel::Place(order))
                        .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;
                    ask_quote = Quote {
                        cloid: Some(cloid),
                        px: Some(ask_px),
                        sz: Some(desired_ask_sz),
                    };
                }
            }

            ctx.emit(Progress::Heartbeat {
                cumulative_filled: all_fills.iter().map(|f| f.sz).sum(),
                remaining: delta,
                ts: Utc::now(),
            })
            .await;
            tokio::time::sleep(params.repost_poll).await;
        }

        let report = build_report(
            ctx.exec_id,
            self.name(),
            started_at,
            target,
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

    fn algo_params_no_freshness(quote_size: Decimal) -> AlgoParams {
        let mut p = AlgoParams::default();
        p.0.insert("max_book_age_ms".into(), serde_json::json!(0));
        p.0.insert(
            "quote_size".into(),
            serde_json::json!(quote_size.to_string()),
        );
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
    fn quote_prices_balanced_around_mid() {
        let (b, a) = quote_prices(dec!(100), dec!(10));
        // 10 bps each side = 0.1
        assert_eq!(b, dec!(99.9));
        assert_eq!(a, dec!(100.1));
    }

    #[test]
    fn quote_sizes_neutral_when_at_target() {
        let (b, a) = quote_sizes(Decimal::ZERO, dec!(1));
        assert_eq!(b, dec!(1));
        assert_eq!(a, dec!(1));
    }

    #[test]
    fn quote_sizes_skew_long() {
        // Need to be longer by 0.5 → bid bigger, ask smaller.
        let (b, a) = quote_sizes(dec!(0.5), dec!(1));
        assert_eq!(b, dec!(1.5));
        assert_eq!(a, dec!(0.5));
    }

    #[test]
    fn quote_sizes_skew_long_above_quote_size_caps_at_2x() {
        let (b, a) = quote_sizes(dec!(5), dec!(1));
        assert_eq!(b, dec!(2)); // capped at quote_size * 2
        assert_eq!(a, dec!(0));
    }

    #[test]
    fn quote_sizes_skew_short() {
        let (b, a) = quote_sizes(dec!(-0.7), dec!(1));
        assert_eq!(b, dec!(0.3));
        assert_eq!(a, dec!(1.7));
    }

    #[test]
    fn from_algo_requires_quote_size() {
        let p = AlgoParams::default();
        assert!(MarketMakeParams::from_algo(&p).is_err());
    }

    #[test]
    fn needs_repost_on_first_quote() {
        let q = Quote::default();
        assert!(MarketMakeAlgorithm::needs_repost(
            &q,
            dec!(100),
            dec!(1),
            dec!(2)
        ));
    }

    #[test]
    fn needs_repost_when_size_changes() {
        let q = Quote {
            cloid: Some(Cloid::new()),
            px: Some(dec!(100)),
            sz: Some(dec!(1)),
        };
        assert!(MarketMakeAlgorithm::needs_repost(
            &q,
            dec!(100),
            dec!(0.5),
            dec!(2)
        ));
    }

    #[test]
    fn no_repost_when_price_moves_below_threshold() {
        let q = Quote {
            cloid: Some(Cloid::new()),
            px: Some(dec!(100)),
            sz: Some(dec!(1)),
        };
        // Move 1 bps (below threshold of 2 bps)
        assert!(!MarketMakeAlgorithm::needs_repost(
            &q,
            dec!(100.01),
            dec!(1),
            dec!(2)
        ));
    }

    #[test]
    fn repost_when_price_moves_above_threshold() {
        let q = Quote {
            cloid: Some(Cloid::new()),
            px: Some(dec!(100)),
            sz: Some(dec!(1)),
        };
        // Move 5 bps (above threshold of 2 bps)
        assert!(MarketMakeAlgorithm::needs_repost(
            &q,
            dec!(100.05),
            dec!(1),
            dec!(2)
        ));
    }

    /// MM places initial bid+ask at 10 bps spread around mid 50000.
    #[tokio::test(start_paused = true)]
    async fn market_make_places_two_sided_alo_at_start() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(49999)).await;

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

        let mut algo = MarketMakeAlgorithm::new();
        // Set target to 0 with current position 0 BUT use a small quote_size
        // so we get neutral quoting with delta=0 → equal bids/asks.
        // To test BOTH-sided placement, we need delta to be within the quote_size
        // skew range (delta.abs() < quote_size). Use target=0, current=0 → delta=0.
        let mut p = algo_params_no_freshness(dec!(0.1));
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(10000));
        p.0.insert("spread_bps_each_side".into(), serde_json::json!("10"));
        // Keep target == position so we exit fast — but we want to see initial
        // quoting before exit. Use a small tolerance that doesn't trigger exit
        // while we still have time to place quotes.
        p.0.insert("target_tolerance_size".into(), serde_json::json!("-1"));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::SetTarget,
            target_size: dec!(0.0),
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
        let _ = abort_tx.send(true);
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        // delta=0 → equal bid/ask sizes → both sides quoted.
        assert!(placed.len() >= 2, "expected ≥2 placed (bid + ask)");
        let bid = placed.iter().find(|o| o.side == Side::Long).unwrap();
        let ask = placed.iter().find(|o| o.side == Side::Short).unwrap();
        // mid = 50000, 10 bps each side → bid 49950, ask 50050
        assert_eq!(bid.px, dec!(49950));
        assert_eq!(ask.px, dec!(50050));
        assert_eq!(bid.tif, Tif::Alo);
        assert_eq!(ask.tif, Tif::Alo);
        assert_eq!(bid.sz, dec!(0.1)); // neutral skew
        assert_eq!(ask.sz, dec!(0.1));
        assert!(report.aborted);

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }

    /// Skew test: target > position → bid bigger, ask smaller (or zero).
    #[tokio::test(start_paused = true)]
    async fn market_make_skews_quotes_when_below_target() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(49999)).await;

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

        let mut algo = MarketMakeAlgorithm::new();
        // delta = 0.05 (within quote_size=0.1) → bid skew: bid_sz=0.15, ask_sz=0.05
        let mut p = algo_params_no_freshness(dec!(0.1));
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(10000));
        p.0.insert("target_tolerance_size".into(), serde_json::json!("-1"));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::SetTarget,
            target_size: dec!(0.05),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..20 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        let _ = abort_tx.send(true);
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }
        let _ = algo_handle.await.expect("join").expect("algo ok");

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let bid = placed.iter().find(|o| o.side == Side::Long).unwrap();
        let ask = placed.iter().find(|o| o.side == Side::Short).unwrap();
        assert_eq!(bid.sz, dec!(0.15));
        assert_eq!(ask.sz, dec!(0.05));

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }

    /// When position moves to target, the algo exits successfully.
    #[tokio::test(start_paused = true)]
    async fn market_make_exits_when_position_reaches_target() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(49999)).await;
        // Start with delta = 0.5 (current = 0.5, target = 1.0)
        set_position(&state, &symbol, dec!(0.5)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // After 100 ms, push a fill on the first bid that brings position to 1.0.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher_state_for_pos = state.clone();
        let watcher_symbol_for_pos = symbol.clone();
        let watcher = tokio::spawn(async move {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let calls = watcher_mock.placed_calls();
                if let Some(latest_batch) = calls.last() {
                    if let Some(o) = latest_batch.iter().find(|o| o.side == Side::Long) {
                        push_fill(
                            &watcher_state,
                            &watcher_symbol,
                            o.cloid,
                            o.side,
                            o.px,
                            dec!(0.5),
                        )
                        .await;
                        // Update position to 1.0
                        set_position(&watcher_state_for_pos, &watcher_symbol_for_pos, dec!(1.0))
                            .await;
                        break;
                    }
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketMakeAlgorithm::new();
        let mut p = algo_params_no_freshness(dec!(0.5));
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        p.0.insert("max_total_ms".into(), serde_json::json!(5000));
        p.0.insert("spread_bps_each_side".into(), serde_json::json!("10"));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::SetTarget,
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
        // Filled at least the 0.5 fill.
        assert!(report.filled_size >= dec!(0.5));
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn market_make_max_total_returns_aborted_when_below_target() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(50001), dec!(49999)).await;
        set_position(&state, &symbol, dec!(0)).await;

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

        let mut algo = MarketMakeAlgorithm::new();
        let mut p = algo_params_no_freshness(dec!(0.1));
        p.0.insert("max_total_ms".into(), serde_json::json!(150));
        p.0.insert("repost_poll_ms".into(), serde_json::json!(40));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::SetTarget,
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
        let cancelled: Vec<_> = mock.cancelled_calls().into_iter().flatten().collect();
        assert!(
            !cancelled.is_empty(),
            "timeout path must cancel resting quotes"
        );
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }
}
