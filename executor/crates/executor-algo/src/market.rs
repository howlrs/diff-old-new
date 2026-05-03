//! `MarketAlgorithm` — taker-style execution.
//!
//! Behavior (design v2 §5):
//! 1. Read top-of-book from `AppState::book`. Reject if missing/stale.
//! 2. Pick a marketable price = best ask + slippage_bps (long) or best bid -
//!    slippage_bps (short). Honors the `max_slippage_bps` AlgoParam (default
//!    20 bps).
//! 3. Submit IOC orders sized at `target_size`. If partially filled, re-quote
//!    against the latest book and submit a top-up order for the remainder.
//! 4. Stop when filled, aborted, slippage cap exceeded, or `max_attempts`
//!    reached.
//!
//! Algo params (`AlgoParams`):
//! - `max_slippage_bps` (Decimal as string, default `"20"`)
//! - `max_attempts` (u32, default `5`)
//! - `slice_timeout_ms` (u32, default `1500`)
//! - `reduce_only` (bool, default `false`)
//! - `fallback_to_taker` (bool, ignored — MARKET is already taker)
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
use executor_core::intent::{ExecutionReport, Intent, OrderIntent, Progress};
use executor_core::state::AppState;
use executor_core::symbol::Symbol;
use executor_core::types::{Side, Tif};

use executor_hl::batch_sender::OrderOrCancel;

use crate::algorithm::{build_report, collect_own_fills, Algorithm, ExecutionContext};

const DEFAULT_MAX_SLIPPAGE_BPS: &str = "20";
const DEFAULT_MAX_ATTEMPTS: u32 = 5;
const DEFAULT_SLICE_TIMEOUT_MS: u32 = 1500;
/// Reject the book if its WS timestamp is older than this. Gemini PR-3 review
/// flagged that a disconnected WS would otherwise let the algo trade against
/// frozen prices.
const DEFAULT_MAX_BOOK_AGE_MS: u32 = 500;

#[derive(Debug, Clone)]
pub struct MarketParams {
    pub max_slippage_bps: Decimal,
    pub max_attempts: u32,
    pub slice_timeout: Duration,
    pub reduce_only: bool,
    /// If the book's timestamp is older than this, the algorithm aborts.
    /// Set to `None` to skip the check (e.g., for unit tests with frozen time).
    pub max_book_age: Option<Duration>,
}

impl MarketParams {
    fn from_algo(p: &executor_core::intent::AlgoParams) -> Result<Self, AlgoError> {
        let max_slippage_bps = p
            .get_decimal("max_slippage_bps")
            .unwrap_or_else(|| DEFAULT_MAX_SLIPPAGE_BPS.parse().unwrap_or(Decimal::ZERO));
        if max_slippage_bps < Decimal::ZERO {
            return Err(AlgoError::InvalidParams(
                "max_slippage_bps must be >= 0".into(),
            ));
        }
        let max_attempts = p.get_u32("max_attempts").unwrap_or(DEFAULT_MAX_ATTEMPTS);
        if max_attempts == 0 {
            return Err(AlgoError::InvalidParams("max_attempts must be > 0".into()));
        }
        let slice_timeout_ms = p
            .get_u32("slice_timeout_ms")
            .unwrap_or(DEFAULT_SLICE_TIMEOUT_MS);
        let reduce_only =
            p.0.get("reduce_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        // `max_book_age_ms = 0` disables the freshness check (handy for tests
        // with `tokio::time::pause()` where wall-clock advances and the
        // book ts looks stale).
        let max_book_age_ms = p
            .get_u32("max_book_age_ms")
            .unwrap_or(DEFAULT_MAX_BOOK_AGE_MS);
        let max_book_age = if max_book_age_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(max_book_age_ms as u64))
        };
        Ok(Self {
            max_slippage_bps,
            max_attempts,
            slice_timeout: Duration::from_millis(slice_timeout_ms as u64),
            reduce_only,
            max_book_age,
        })
    }
}

/// Decide trade side based on `Intent` + signed `target_size` + current
/// `Position`. Returns the (Side, abs_size_to_trade).
///
/// - `Open` / `SetTarget`: side = sign(target_size), size = |target_size - current|
/// - `Close`: opposite of current position, size = min(|target|, |current|)
fn resolve_side_and_size(
    intent: Intent,
    target_size: Decimal,
    current_size: Decimal,
) -> Result<(Side, Decimal), AlgoError> {
    match intent {
        Intent::Open => {
            if target_size == Decimal::ZERO {
                return Err(AlgoError::InvalidParams("Open with target_size = 0".into()));
            }
            let side = if target_size > Decimal::ZERO {
                Side::Long
            } else {
                Side::Short
            };
            Ok((side, target_size.abs()))
        }
        Intent::SetTarget => {
            let delta = target_size - current_size;
            if delta == Decimal::ZERO {
                return Err(AlgoError::InvalidParams("already at target".into()));
            }
            let side = if delta > Decimal::ZERO {
                Side::Long
            } else {
                Side::Short
            };
            Ok((side, delta.abs()))
        }
        Intent::Close => {
            if current_size == Decimal::ZERO {
                return Err(AlgoError::InvalidParams("no position to close".into()));
            }
            let side = if current_size > Decimal::ZERO {
                Side::Short
            } else {
                Side::Long
            };
            let abs = if target_size == Decimal::ZERO {
                current_size.abs()
            } else {
                target_size.abs().min(current_size.abs())
            };
            Ok((side, abs))
        }
    }
}

/// Reject the book if its WS timestamp is missing or older than `max_age`.
/// Gemini PR-3 review: a disconnected WS would otherwise leave a stale book
/// in `AppState`, and the algo would happily trade against frozen prices.
fn ensure_book_fresh(
    book: &executor_core::state::OrderBook,
    max_age: Option<Duration>,
) -> Result<(), AlgoError> {
    let Some(max_age) = max_age else {
        return Ok(());
    };
    let ts = book
        .ts
        .ok_or_else(|| AlgoError::InvalidParams("market: book has no ts".into()))?;
    let age = Utc::now() - ts;
    let age_ms = age.num_milliseconds();
    if age_ms < 0 {
        // ts in the future — clock skew or tests with paused-time. Treat as
        // fresh; the caller can disable the check via max_book_age=0.
        return Ok(());
    }
    if (age_ms as u128) > max_age.as_millis() {
        return Err(AlgoError::InvalidParams(format!(
            "market: book stale ({age_ms}ms > {}ms)",
            max_age.as_millis()
        )));
    }
    Ok(())
}

/// Compute the IOC limit price including slippage cap.
///
/// For a Long taker, price = best_ask * (1 + slippage_bps / 10000)
/// For a Short taker, price = best_bid * (1 - slippage_bps / 10000)
fn compute_limit_price(
    book: &executor_core::state::OrderBook,
    side: Side,
    slippage_bps: Decimal,
) -> Result<Decimal, AlgoError> {
    let bps_factor = slippage_bps / Decimal::from(10_000);
    match side {
        Side::Long => {
            let ask = book.best_ask().ok_or_else(|| {
                AlgoError::InvalidParams("market: empty asks (no best ask)".into())
            })?;
            Ok(ask + ask * bps_factor)
        }
        Side::Short => {
            let bid = book.best_bid().ok_or_else(|| {
                AlgoError::InvalidParams("market: empty bids (no best bid)".into())
            })?;
            Ok(bid - bid * bps_factor)
        }
    }
}

#[derive(Debug, Default)]
pub struct MarketAlgorithm;

impl MarketAlgorithm {
    pub fn new() -> Self {
        Self
    }

    /// Snapshot current top-of-book for a symbol.
    async fn snapshot_book(
        &self,
        state: &Arc<AppState>,
        symbol: &Symbol,
    ) -> Result<executor_core::state::OrderBook, AlgoError> {
        let book = state.book.read().await;
        book.get(symbol)
            .cloned()
            .ok_or_else(|| AlgoError::InvalidParams(format!("no book for {symbol}")))
    }

    /// Snapshot current position size for a symbol (zero if absent).
    async fn snapshot_position_size(&self, state: &Arc<AppState>, symbol: &Symbol) -> Decimal {
        let pos = state.position.read().await;
        pos.get(symbol).map(|p| p.size).unwrap_or(Decimal::ZERO)
    }
}

#[async_trait::async_trait]
impl Algorithm for MarketAlgorithm {
    fn name(&self) -> &'static str {
        "MARKET"
    }

    async fn run(&mut self, ctx: ExecutionContext) -> Result<ExecutionReport, AlgoError> {
        let started_at = Utc::now();
        let params = MarketParams::from_algo(&ctx.params)?;

        ctx.emit(Progress::Started {
            exec_id: ctx.exec_id,
            ts: started_at,
        })
        .await;

        // Decide side + total size to trade.
        let current = self.snapshot_position_size(&ctx.state, &ctx.symbol).await;
        let (side, abs_size) = resolve_side_and_size(ctx.intent, ctx.target_size, current)?;
        if abs_size <= Decimal::ZERO {
            return Err(AlgoError::InvalidParams("derived size <= 0".into()));
        }

        let mut remaining = abs_size;
        let mut own_cloids: HashSet<Cloid> = HashSet::new();
        let mut all_fills = Vec::new();
        let mut slice = 0u32;
        let mut abort_rx = ctx.abort.clone();

        while remaining > Decimal::ZERO {
            if *abort_rx.borrow() {
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
            slice += 1;
            if slice > params.max_attempts {
                return Ok(build_report(
                    ctx.exec_id,
                    self.name(),
                    started_at,
                    abs_size,
                    all_fills,
                    true,
                    Some(format!(
                        "max_attempts ({}) exceeded with {} remaining",
                        params.max_attempts, remaining
                    )),
                ));
            }

            // Re-snapshot book for each attempt (price may have moved).
            let book = self.snapshot_book(&ctx.state, &ctx.symbol).await?;
            // Gemini PR-3 review: fail loud on stale book rather than trade
            // against frozen WS prices.
            ensure_book_fresh(&book, params.max_book_age)?;
            let limit_px = compute_limit_price(&book, side, params.max_slippage_bps)?;

            let cloid = Cloid::new();
            own_cloids.insert(cloid);
            let order = OrderIntent {
                cloid,
                symbol: ctx.symbol.clone(),
                side,
                px: limit_px,
                sz: remaining,
                tif: Tif::Ioc,
                reduce_only: params.reduce_only,
            };
            tracing::debug!(
                slice = slice,
                px = %order.px,
                sz = %order.sz,
                cloid = %cloid,
                "market: enqueue IOC slice"
            );
            ctx.batch
                .enqueue(OrderOrCancel::Place(order))
                .map_err(|e| AlgoError::HyperliquidError(format!("batch enqueue: {e}")))?;

            // Wait for fills (or timeout).
            let (slice_fills, _ok) = collect_own_fills(
                &ctx.state,
                &own_cloids,
                abs_size, // collected so far should reach the original target
                Duration::from_millis(50),
                params.slice_timeout,
                &mut abort_rx,
            )
            .await;

            let new_fills: Vec<_> = slice_fills
                .iter()
                .filter(|f| {
                    !all_fills
                        .iter()
                        .any(|existing: &executor_core::types::Fill| existing.oid == f.oid)
                })
                .cloned()
                .collect();
            let slice_filled: Decimal = new_fills.iter().map(|f| f.sz).sum();

            for f in &new_fills {
                ctx.emit(Progress::SliceFilled {
                    slice,
                    cloid,
                    px: f.px,
                    sz: f.sz,
                    cumulative_filled: all_fills.iter().map(|x| x.sz).sum::<Decimal>() + f.sz,
                })
                .await;
            }
            all_fills.extend(new_fills);
            remaining = (remaining - slice_filled).max(Decimal::ZERO);
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
    use executor_core::cloid::Cloid;
    use executor_core::intent::{AlgoParams, ExecutionId};
    use executor_core::state::{BookLevel, OrderBook, Position};
    use executor_core::types::{Fill, OrderId};
    use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
    use executor_hl::hl_client::MockHlClient;
    use rust_decimal_macros::dec;
    use std::sync::Arc;
    use tokio::sync::{mpsc, watch};

    fn lvl(px: Decimal, sz: Decimal) -> BookLevel {
        BookLevel { px, sz, n: 1 }
    }

    /// `tokio::time::pause()` freezes virtual time, but `Utc::now()` keeps
    /// walking — so the seeded book ts looks stale. Tests that drive the
    /// algorithm async loop disable the freshness check to compensate.
    fn algo_params_no_freshness() -> AlgoParams {
        let mut p = AlgoParams::default();
        p.0.insert("max_book_age_ms".into(), serde_json::json!(0));
        p
    }

    fn seed_book(state: &Arc<AppState>, symbol: &Symbol, ask: Decimal, bid: Decimal) {
        let book = OrderBook {
            bids: vec![lvl(bid, dec!(10))],
            asks: vec![lvl(ask, dec!(10))],
            ts: Some(Utc::now()),
        };
        let s = state.clone();
        let symbol = symbol.clone();
        // synchronous wrap
        futures::executor::block_on(async move {
            let mut b = s.book.write().await;
            b.insert(symbol, book);
        });
    }

    fn seed_position(state: &Arc<AppState>, symbol: &Symbol, size: Decimal) {
        let s = state.clone();
        let symbol = symbol.clone();
        futures::executor::block_on(async move {
            let mut g = s.position.write().await;
            g.insert(
                symbol,
                Position {
                    size,
                    ..Default::default()
                },
            );
        });
    }

    fn push_fill(
        state: &Arc<AppState>,
        symbol: &Symbol,
        cloid: Cloid,
        side: Side,
        px: Decimal,
        sz: Decimal,
    ) {
        let s = state.clone();
        let symbol = symbol.clone();
        futures::executor::block_on(async move {
            let mut f = s.recent_fills.write().await;
            let next_oid = f.len() as u64 + 1;
            f.push_back(Fill {
                symbol,
                cloid: Some(cloid),
                oid: OrderId(next_oid),
                side,
                px,
                sz,
                fee: Decimal::ZERO,
                ts: Utc::now(),
            });
        });
    }

    #[test]
    fn resolve_side_open_long() {
        let (s, abs) = resolve_side_and_size(Intent::Open, dec!(2.5), dec!(0)).expect("resolve");
        assert_eq!(s, Side::Long);
        assert_eq!(abs, dec!(2.5));
    }

    #[test]
    fn resolve_side_open_short() {
        let (s, abs) = resolve_side_and_size(Intent::Open, dec!(-1.0), dec!(0)).expect("resolve");
        assert_eq!(s, Side::Short);
        assert_eq!(abs, dec!(1.0));
    }

    #[test]
    fn resolve_side_close_long_position() {
        let (s, abs) = resolve_side_and_size(Intent::Close, dec!(0), dec!(3.0)).expect("resolve");
        assert_eq!(s, Side::Short);
        assert_eq!(abs, dec!(3.0));
    }

    #[test]
    fn resolve_side_close_partial_short() {
        let (s, abs) =
            resolve_side_and_size(Intent::Close, dec!(1.0), dec!(-2.0)).expect("resolve");
        assert_eq!(s, Side::Long);
        assert_eq!(abs, dec!(1.0));
    }

    #[test]
    fn resolve_side_set_target_increase_long() {
        let (s, abs) =
            resolve_side_and_size(Intent::SetTarget, dec!(5.0), dec!(2.0)).expect("resolve");
        assert_eq!(s, Side::Long);
        assert_eq!(abs, dec!(3.0));
    }

    #[test]
    fn resolve_side_close_with_no_position_errors() {
        assert!(resolve_side_and_size(Intent::Close, dec!(1), dec!(0)).is_err());
    }

    #[test]
    fn limit_price_long_includes_slippage() {
        let book = OrderBook {
            bids: vec![lvl(dec!(99), dec!(1))],
            asks: vec![lvl(dec!(100), dec!(1))],
            ts: None,
        };
        // 50 bps slippage on 100 = 0.5
        let px = compute_limit_price(&book, Side::Long, dec!(50)).unwrap();
        assert_eq!(px, dec!(100.5));
    }

    #[test]
    fn limit_price_short_includes_slippage() {
        let book = OrderBook {
            bids: vec![lvl(dec!(100), dec!(1))],
            asks: vec![lvl(dec!(101), dec!(1))],
            ts: None,
        };
        let px = compute_limit_price(&book, Side::Short, dec!(50)).unwrap();
        assert_eq!(px, dec!(99.5));
    }

    #[tokio::test(start_paused = true)]
    async fn market_open_long_full_fill_via_mock() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(50000), dec!(49999));

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Watcher pushes a fill into recent_fills shortly after the batch
        // flush so MarketAlgorithm sees it as "filled".
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                if let Some(latest) = calls.last() {
                    for o in latest {
                        let already_pushed = {
                            let f = watcher_state.recent_fills.read().await;
                            f.iter().any(|fill| fill.cloid == Some(o.cloid))
                        };
                        if !already_pushed {
                            push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz);
                        }
                    }
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketAlgorithm::new();
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.5),
            params: algo_params_no_freshness(),
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        // Drive forward in fake time.
        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });

        // Advance time enough for the flusher + watcher to act several times.
        for _ in 0..30 {
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
        }

        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted);
        assert_eq!(report.filled_size, dec!(0.5));

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn market_close_short_uses_long_side() {
        let symbol = Symbol::new("ETH");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(2000), dec!(1999));
        seed_position(&state, &symbol, dec!(-1.0)); // short 1 ETH

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
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = watcher_mock.placed_calls();
                if let Some(latest) = calls.last() {
                    for o in latest {
                        let already_pushed = {
                            let f = watcher_state.recent_fills.read().await;
                            f.iter().any(|fill| fill.cloid == Some(o.cloid))
                        };
                        if !already_pushed {
                            push_fill(&watcher_state, &watcher_symbol, o.cloid, o.side, o.px, o.sz);
                        }
                    }
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketAlgorithm::new();
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Close,
            target_size: Decimal::ZERO,
            params: algo_params_no_freshness(),
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
        assert!(!report.aborted);
        assert_eq!(report.filled_size, dec!(1.0));
        let placed = mock.placed_calls();
        assert!(!placed.is_empty());
        let first = &placed[0][0];
        assert_eq!(first.side, Side::Long); // closing a short = buy back
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn market_aborts_when_signaled() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(100), dec!(99));

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

        // Abort immediately.
        let _ = abort_tx.send(true);

        let mut algo = MarketAlgorithm::new();
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(1.0),
            params: AlgoParams::default(),
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

    #[tokio::test(start_paused = true)]
    async fn market_returns_aborted_when_max_attempts_exhausted() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(100), dec!(99));

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );
        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("max_attempts".into(), serde_json::json!(2));
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(50));
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
            .map(|r| r.contains("max_attempts"))
            .unwrap_or(false));
        assert_eq!(report.filled_size, Decimal::ZERO);
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }

    /// Gemini PR-3 review #1: stale book → algo aborts before sending order.
    #[test]
    fn ensure_book_fresh_rejects_stale() {
        let stale = OrderBook {
            bids: vec![lvl(dec!(99), dec!(1))],
            asks: vec![lvl(dec!(100), dec!(1))],
            // 2 seconds in the past
            ts: Some(Utc::now() - chrono::Duration::seconds(2)),
        };
        let res = ensure_book_fresh(&stale, Some(Duration::from_millis(500)));
        assert!(res.is_err());
        let err = format!("{}", res.unwrap_err());
        assert!(err.contains("stale"));
    }

    #[test]
    fn ensure_book_fresh_accepts_recent() {
        let fresh = OrderBook {
            bids: vec![lvl(dec!(99), dec!(1))],
            asks: vec![lvl(dec!(100), dec!(1))],
            ts: Some(Utc::now()),
        };
        assert!(ensure_book_fresh(&fresh, Some(Duration::from_millis(500))).is_ok());
    }

    #[test]
    fn ensure_book_fresh_disabled_with_none() {
        let stale = OrderBook {
            bids: vec![lvl(dec!(99), dec!(1))],
            asks: vec![lvl(dec!(100), dec!(1))],
            ts: Some(Utc::now() - chrono::Duration::seconds(60)),
        };
        // Skip the check entirely.
        assert!(ensure_book_fresh(&stale, None).is_ok());
    }

    #[test]
    fn ensure_book_fresh_rejects_missing_ts() {
        let no_ts = OrderBook {
            bids: vec![lvl(dec!(99), dec!(1))],
            asks: vec![lvl(dec!(100), dec!(1))],
            ts: None,
        };
        assert!(ensure_book_fresh(&no_ts, Some(Duration::from_millis(500))).is_err());
    }

    /// Gemini PR-3 review-2 follow-up: two consecutive partial fills should
    /// chain into a third slice that finishes the remainder. Verifies the
    /// loop tracks cumulative remainder correctly across multiple slices.
    #[tokio::test(start_paused = true)]
    async fn market_chains_multiple_partial_fills() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(50000), dec!(49999));

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Watcher: first slice fills 50%, second slice fills 50% of its
        // remainder (= 25% of original), third slice fills in full.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            let mut slice_idx = 0u32;
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
                    slice_idx += 1;
                    let fill_sz = match slice_idx {
                        1 | 2 => o.sz * dec!(0.5), // 50% partial
                        _ => o.sz,                 // full
                    };
                    push_fill(
                        &watcher_state,
                        &watcher_symbol,
                        o.cloid,
                        o.side,
                        o.px,
                        fill_sz,
                    );
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(300));
        p.0.insert("max_attempts".into(), serde_json::json!(10));
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
        for _ in 0..400 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(1.0));
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let unique_cloids: HashSet<_> = placed.iter().map(|o| o.cloid).collect();
        assert!(
            unique_cloids.len() >= 3,
            "expected ≥3 slices for two partial fills, got {}",
            unique_cloids.len()
        );
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }

    /// Gemini PR-3 review #2: a partial fill on slice 1 should produce a
    /// top-up order on slice 2 sized at the remainder, and complete cleanly.
    #[tokio::test(start_paused = true)]
    async fn market_top_ups_after_partial_fill() {
        let symbol = Symbol::new("BTC");
        let state = Arc::new(AppState::new());
        seed_book(&state, &symbol, dec!(50000), dec!(49999));

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Watcher: for the first observed cloid, fill only 60 % (partial).
        // For all subsequent cloids, fill in full. This forces the algo to
        // detect a remainder and submit a second slice.
        let watcher_state = state.clone();
        let watcher_mock = mock.clone();
        let watcher_symbol = symbol.clone();
        let watcher = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            let mut first_cloid: Option<Cloid> = None;
            for iter in 0..200 {
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
                    let is_first = first_cloid.is_none();
                    if is_first {
                        first_cloid = Some(o.cloid);
                    }
                    let fill_sz = if is_first {
                        // 60 % partial fill on the first observed cloid
                        o.sz * dec!(0.6)
                    } else {
                        // full fill on subsequent slices
                        o.sz
                    };
                    let _ = iter; // silence unused-var warning when no debug print
                    push_fill(
                        &watcher_state,
                        &watcher_symbol,
                        o.cloid,
                        o.side,
                        o.px,
                        fill_sz,
                    );
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = MarketAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("slice_timeout_ms".into(), serde_json::json!(300));
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
        assert_eq!(report.filled_size, dec!(1.0), "should top up to full size");
        // Two slices = two distinct cloids submitted.
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        let unique_cloids: HashSet<_> = placed.iter().map(|o| o.cloid).collect();
        assert!(
            unique_cloids.len() >= 2,
            "expected ≥2 slices, got {}",
            unique_cloids.len()
        );
        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
    }
}
