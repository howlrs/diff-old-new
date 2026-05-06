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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

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
/// PR-D3: in-flight cap epsilon. Sizes ≤ this are treated as fully cleared.
/// 0.0001 covers HL minimum order sizes for all current perp symbols.
const IN_FLIGHT_EPS: Decimal = dec!(0.0001);
/// PR-D3: how long an enqueued place may stay invisible to `state.open_orders`
/// before we conclude the order was rejected (or the WS feed lost it) and drop
/// it from `local_in_flight`. 10 s is a comfortable upper bound on the HL ack
/// path even under load.
const IN_FLIGHT_REJECT_TIMEOUT: Duration = Duration::from_secs(10);

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

/// PR-D3: per-cloid in-flight bookkeeping owned entirely by the algorithm.
/// Kept off `state.open_orders` so the WS path remains the single source of
/// truth for "what HL says is resting"; this struct is purely the algorithm's
/// view of "what I asked for and haven't fully accounted for yet".
#[derive(Debug, Clone)]
struct InFlight {
    /// Outstanding size (place size minus fills observed via `recent_fills`).
    sz: Decimal,
    /// `true` once we've observed this cloid in `state.open_orders`. A
    /// subsequent disappearance then implies Cancel or Full Fill — safe to
    /// drop. While `false`, we rely on the reject-timeout guard.
    seen_open: bool,
    /// `tokio::time::Instant` so unit tests using `start_paused = true` can
    /// drive the reject-timeout deterministically.
    placed_at: tokio::time::Instant,
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
        // PR-D3: defense-in-depth against the PR-D1 mainnet incident
        // (WS feed broken → algo never sees own fills → repost loop accumulates
        // 7× the target position). We track per-cloid speculative size locally
        // and refuse to place a new quote while any of our previous quotes
        // could still be resting on HL. Cleanup observes `state.open_orders`
        // (so a healthy WS path drains entries the moment HL acks Cancel/Fill)
        // and falls back to a wall-clock timeout for orders the WS feed never
        // surfaces (rejects, dropped frames). The fail-safe shape is:
        //   WS healthy   → entry removed quickly, normal reposting resumes
        //   WS broken    → entry persists, place is blocked until BaselineGuard
        //                  or operator intervenes (silent stop, never a leak).
        let mut local_in_flight: HashMap<Cloid, InFlight> = HashMap::new();
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

            // PR-D3: reconcile local_in_flight against the WS-fed open_orders
            // map and the wall-clock reject timeout. See `InFlight` for the
            // intended fail-safe shape.
            {
                let now = tokio::time::Instant::now();
                let open_now = ctx.state.open_orders.read().await;
                // Collect cloids we are about to drop because of the reject
                // timeout — we still want to fire a defensive Cancel so a
                // late-arriving order can't linger as a "zombie" on HL.
                let mut zombie_cancels: Vec<Cloid> = Vec::new();
                local_in_flight.retain(|cloid, entry| {
                    if open_now.contains_key(cloid) {
                        entry.seen_open = true;
                        return true;
                    }
                    if entry.seen_open {
                        // Was visible to WS, now gone — Cancel or Full Fill
                        // settled. Drop.
                        return false;
                    }
                    if now.saturating_duration_since(entry.placed_at) >= IN_FLIGHT_REJECT_TIMEOUT {
                        tracing::warn!(
                            %cloid,
                            sz = %entry.sz,
                            "passive_follow: in-flight entry never appeared in open_orders \
                             (suspected reject / dropped WS), firing defensive cancel"
                        );
                        zombie_cancels.push(*cloid);
                        return false;
                    }
                    true
                });
                // Defensive cancels for the timed-out entries: if HL silently
                // accepted the order after our 10 s window, this prevents a
                // "ghost resting" from accumulating across repeated timeouts.
                // Unknown cloids on HL are a no-op so this is always safe.
                for c in zombie_cancels {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }
                // If we dropped the cloid we were treating as our active
                // quote, forget it — there is nothing on HL to cancel and
                // we want the next iteration to repost even if the touch
                // hasn't moved.
                if let Some((c, _)) = current_quote {
                    if !local_in_flight.contains_key(&c) {
                        current_quote = None;
                    }
                }
            }

            // Drain any fills since last poll.
            let (new_fills, new_sz, idx) =
                drain_new_fills(&ctx.state, &own_cloids, last_fill_idx).await;
            last_fill_idx = idx;
            for f in &new_fills {
                // Reduce the speculative in-flight bucket by the observed size.
                // Drop entries whose remainder fell to ≤ EPS.
                if let Some(c) = f.cloid {
                    if let Some(entry) = local_in_flight.get_mut(&c) {
                        entry.sz -= f.sz;
                        if entry.sz <= IN_FLIGHT_EPS {
                            local_in_flight.remove(&c);
                        }
                    }
                }
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
                // PR-D3: in-flight cap. If any **prior** place (one we no
                // longer hold as `current_quote`) is still outstanding —
                // because WS hasn't echoed Cancel/Fill yet and the reject
                // timeout hasn't fired — refuse to layer another resting
                // order on top. The current quote itself is excluded: the
                // very next step is to cancel + repost it, which is a normal
                // 1-for-1 swap rather than a runaway. The check sits AFTER
                // `drain_new_fills` and the open_orders reconcile so a
                // healthy WS path naturally clears the bucket between ticks.
                let current_cloid = current_quote.as_ref().map(|(c, _)| *c);
                let in_flight_total: Decimal = local_in_flight
                    .iter()
                    .filter(|(c, _)| Some(**c) != current_cloid)
                    .map(|(_, e)| e.sz)
                    .sum();
                if in_flight_total > IN_FLIGHT_EPS {
                    tracing::warn!(
                        in_flight = %in_flight_total,
                        target = %abs_size,
                        outstanding = local_in_flight.len(),
                        "passive_follow: in-flight cap reached, skipping place this tick"
                    );
                    tokio::time::sleep(params.repost_poll).await;
                    continue;
                }

                // PR-D10: cancel+place race による target 超過対策。
                // current_quote (これから cancel する cloid) が **HL 側で
                // 確実に resting している** (= seen_open=true) かつ部分 fill 後
                // の残量を抱えている場合、その残量を差し引いた上で新 ALO を
                // 出す。race で c1+c2 両方が約定しても合計 = remaining ≤ target
                // となり超過は起きない。
                //
                // seen_open=false の場合は HL に届いていない可能性 (= reject /
                // dropped frame) が高く、PR-D3 の reject_timeout 路線で扱う方が
                // 安全 (race そのものが起きない)。`sz` は引かず従来挙動を維持。
                //
                // &current_quote で借用、後段の take() を壊さない (Gemini deep
                // review 指摘)。
                let prev_in_flight_sz: Decimal = current_quote
                    .as_ref()
                    .and_then(|(cloid, _)| local_in_flight.get(cloid))
                    .filter(|e| e.seen_open)
                    .map(|e| e.sz)
                    .unwrap_or(Decimal::ZERO);
                let new_sz = (remaining - prev_in_flight_sz).max(Decimal::ZERO);

                // Cancel the previous quote (if any) — **常に発行**する。
                // Gemini deep review (2026-05-06) 指摘: cancel まで skip すると
                // 市場が動いても古い注文が残り続け、価格追従が永久停止する。
                // 新 place は new_sz 判定で個別に skip できるが、cancel は触
                // 移動を観測した時点で必ず enqueue。
                if let Some((c, _)) = current_quote.take() {
                    let _ = ctx.batch.enqueue(OrderOrCancel::Cancel(CancelIntent {
                        symbol: ctx.symbol.clone(),
                        by_cloid: Some(c),
                        by_oid: None,
                    }));
                }

                // 新規 place は new_sz が EPS を超える場合のみ。EPS 以下なら
                // 既存 quote の残量だけで target が埋まる見込みのため、
                // 次 tick で local_in_flight が drain されてから再判定する。
                if new_sz <= IN_FLIGHT_EPS {
                    tracing::warn!(
                        prev_in_flight = %prev_in_flight_sz,
                        remaining = %remaining,
                        "passive_follow: skipping place to avoid target overshoot, \
                         waiting for cancel to clear"
                    );
                    tokio::time::sleep(params.repost_poll).await;
                    continue;
                }

                let cloid = Cloid::new();
                own_cloids.insert(cloid);
                let order = OrderIntent {
                    cloid,
                    symbol: ctx.symbol.clone(),
                    side,
                    px: new_touch,
                    sz: new_sz,
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
                // Only record the in-flight entry once enqueue has succeeded —
                // otherwise a permanently-failed channel would leave a ghost
                // entry blocking every future place.
                local_in_flight.insert(
                    cloid,
                    InFlight {
                        // PR-D10: 新 place の outstanding は new_sz (差し引き後)。
                        sz: new_sz,
                        seen_open: false,
                        placed_at: tokio::time::Instant::now(),
                    },
                );
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

    /// PR-D3 regression: when WS is broken (no fills, no order_update Open
    /// echo) the algo must NOT keep stacking new ALOs on top of its own
    /// resting order. Once a place has been enqueued and the prior cloid is
    /// observed in `state.open_orders`, every subsequent tick must be skipped
    /// by the in-flight cap until either a fill arrives, the order leaves
    /// `open_orders` (Cancel/Fill), or the reject timeout elapses.
    #[tokio::test(start_paused = true)]
    async fn passive_follow_in_flight_cap_blocks_overplace_when_ws_dead() {
        use executor_core::state::OpenOrder;

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

        // Simulate a healthy enough WS that the FIRST place gets echoed back
        // into open_orders (so the in-flight entry flips to seen_open=true)
        // — but no fills, no cancel echoes. This is exactly the PR-D1
        // mainnet failure mode minus the agent/master subscription bug.
        let echoer_state = state.clone();
        let echoer_mock = mock.clone();
        let echoer_symbol = symbol.clone();
        let echoer = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            for _ in 0..500 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = echoer_mock.placed_calls();
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
                    let mut g = echoer_state.open_orders.write().await;
                    g.insert(
                        o.cloid,
                        OpenOrder {
                            cloid: o.cloid,
                            oid: None,
                            symbol: echoer_symbol.clone(),
                            side: o.side,
                            px: o.px,
                            sz: o.sz,
                            filled_sz: Decimal::ZERO,
                            tif: o.tif,
                            reduce_only: o.reduce_only,
                            placed_at: Utc::now(),
                        },
                    );
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        // Total budget shorter than IN_FLIGHT_REJECT_TIMEOUT (10 s) so the
        // run finishes via max_total, not via the reject-timeout drop.
        p.0.insert("max_total_ms".into(), serde_json::json!(2000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.005),
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
        // The run times out (no fills happened). The point is: the cap kept
        // us from placing 7× the target, exactly the PR-D1 incident shape.
        assert!(report.aborted, "expected timeout abort");
        assert_eq!(report.filled_size, Decimal::ZERO);

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        // Without the cap, repost_poll=50ms over 2s would yield ~40 places.
        // With the cap, we expect exactly one resting place to be live at
        // any time. We tolerate ≤2 because the very first tick may place
        // before the echoer has had a chance to populate open_orders.
        assert!(
            placed.len() <= 2,
            "in-flight cap must prevent runaway reposts; got {} placed",
            placed.len()
        );

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        echoer.abort();
    }

    /// PR-D3 regression: an order that is enqueued but never appears in
    /// `state.open_orders` (HL reject, dropped WS frame) must be dropped from
    /// `local_in_flight` after `IN_FLIGHT_REJECT_TIMEOUT`, releasing the cap
    /// and unblocking subsequent reposts. Without this fail-safe the algo
    /// would freeze permanently on the first reject.
    #[tokio::test(start_paused = true)]
    async fn passive_follow_in_flight_cap_recovers_after_reject_timeout() {
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

        // No echoer, no watcher. Every place vanishes into HL with no echo
        // back into either `open_orders` or `recent_fills` — modeling a
        // reject / dropped frame.
        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        // Long enough that the reject timeout (10 s) fires and at least one
        // fresh repost happens after the recovery.
        p.0.insert("max_total_ms".into(), serde_json::json!(15000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: dec!(0.005),
            params: p,
            state: state.clone(),
            batch: batch_sender,
            progress: progress_tx,
            abort: abort_rx,
        };

        let algo_handle = tokio::spawn(async move { algo.run(ctx).await });
        for _ in 0..1500 {
            tokio::time::advance(Duration::from_millis(20)).await;
            tokio::task::yield_now().await;
        }
        let report = algo_handle.await.expect("join").expect("algo ok");
        assert!(report.aborted, "expected timeout (no fills ever happened)");

        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        // Reject-timeout-driven recovery should produce ≥2 places over 15s
        // (one initial + at least one after the 10s timeout). And it must
        // NOT runaway — far fewer than the unbounded 15s/50ms ≈ 300.
        assert!(
            placed.len() >= 2,
            "reject timeout must release the cap; got {} placed",
            placed.len()
        );
        assert!(
            placed.len() <= 5,
            "cap must still suppress runaway after recovery; got {} placed",
            placed.len()
        );

        // Defensive cancel must fire for the zombie cloid so a late-arrival
        // on HL can't accumulate as a ghost resting order — see Gemini deep
        // review (2026-05-05) §"ゴースト化" risk.
        let cancelled: Vec<_> = mock.cancelled_calls().into_iter().flatten().collect();
        assert!(
            !cancelled.is_empty(),
            "reject timeout must dispatch a defensive cancel for the zombie cloid"
        );

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
    }

    /// PR-D10 regression: cancel+place race による target 超過の防御。
    /// 2026-05-06 mainnet HYPE build round 7 で実発生したケースを再現する。
    ///
    /// 構成:
    /// 1. c1 を `state.open_orders` に echo (seen_open=true)
    /// 2. c1 が partial 0.3 fill (target 1.0 中 0.3 約定)
    /// 3. touch を動かして algo に repost を強制
    ///
    /// 期待: 新 place の sz は `remaining (0.7) - prev_in_flight_sz (0.7) = 0`
    ///       で EPS 以下 → place skip。c1 の cancel は出る。これにより HL 側
    ///       で c1+c2 race が起きても合計 ≤ target が **構造的に保証** される。
    #[tokio::test(start_paused = true)]
    async fn passive_follow_race_cap_skips_place_after_partial_fill_and_repost() {
        use executor_core::state::OpenOrder;

        let symbol = Symbol::new("HYPE");
        let state = Arc::new(AppState::new());
        set_book(&state, &symbol, dec!(43.97), dec!(43.96)).await;

        let mock = Arc::new(MockHlClient::new());
        let (batch_sender, handle) = spawn_batch_sender(
            mock.clone(),
            BatchSenderConfig {
                flush_interval: Duration::from_millis(20),
                max_batch_size: 10,
            },
        );

        // Mover: 200ms 後に bid を 1 tick 上げて algo に repost を要求する。
        let mover_state = state.clone();
        let mover_symbol = symbol.clone();
        let mover = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            set_book(&mover_state, &mover_symbol, dec!(43.97), dec!(43.965)).await;
        });

        // Echoer: 全ての placed cloid を open_orders に push (seen_open=true 化)。
        // 最初の cloid (c1) には partial 0.3 fill を 1 度だけ流す。後続の
        // place が来てもそれは絶対に約定させない (検証対象は c2 が place
        // されるか否か)。
        let echoer_state = state.clone();
        let echoer_mock = mock.clone();
        let echoer_symbol = symbol.clone();
        let target = dec!(1.0);
        let partial = dec!(0.3);
        let echoer = tokio::spawn(async move {
            let mut handled: HashSet<Cloid> = HashSet::new();
            let mut filled_partial = false;
            for _ in 0..500 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let calls = echoer_mock.placed_calls();
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
                    let mut g = echoer_state.open_orders.write().await;
                    g.insert(
                        o.cloid,
                        OpenOrder {
                            cloid: o.cloid,
                            oid: None,
                            symbol: echoer_symbol.clone(),
                            side: o.side,
                            px: o.px,
                            sz: o.sz,
                            filled_sz: Decimal::ZERO,
                            tif: o.tif,
                            reduce_only: o.reduce_only,
                            placed_at: Utc::now(),
                        },
                    );
                    drop(g);
                    if !filled_partial {
                        push_fill(
                            &echoer_state,
                            &echoer_symbol,
                            o.cloid,
                            o.side,
                            o.px,
                            partial,
                        )
                        .await;
                        filled_partial = true;
                    }
                }
            }
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<Progress>(64);
        let (_abort_tx, abort_rx) = watch::channel(false);

        let mut algo = PassiveFollowAlgorithm::new();
        let mut p = algo_params_no_freshness();
        p.0.insert("repost_poll_ms".into(), serde_json::json!(50));
        // Total: race が顕在化する 2 秒だけ走らせる (cancel ack のシミュレート無し
        // のため reject_timeout には届かない)。
        p.0.insert("max_total_ms".into(), serde_json::json!(2000));
        let ctx = ExecutionContext {
            exec_id: ExecutionId::new(),
            symbol: symbol.clone(),
            intent: Intent::Open,
            target_size: target,
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
        // partial 0.3 のみ約定。残 0.7 は race cap で place skip → max_total
        // で abort が正常な期待動作。
        assert!(report.aborted, "expected timeout abort");
        assert_eq!(report.filled_size, partial, "only the partial should fill");

        // PR-D10 の核心: c1 だけが置かれ、c2 (race の元) は EPS 判定で
        // skip されているはず。`placed.len() == 1`。
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        assert_eq!(
            placed.len(),
            1,
            "race cap must skip c2 place after partial fill; got {} placed",
            placed.len()
        );
        // 累積 = 0 < target (= 1.0 の 100% 約定にはならない)。
        // race が起きていたら 0.3 + 0.7 + place(c2 sz=0.7) → 約定して target
        // を超える可能性があった (mainnet 2026-05-06 r7 の事象と同型)。
        // 本テストは「c2 が出ない」= race の元が断たれていることを検証する。
        assert!(
            mock.placed_calls()
                .into_iter()
                .flatten()
                .all(|o| o.sz <= target),
            "no individual place may exceed the original target"
        );
        // touch 移動への追従として cancel は最低 1 件出ているはず
        // (Gemini deep review: cancel skip 禁止)。
        let cancelled: Vec<_> = mock.cancelled_calls().into_iter().flatten().collect();
        assert!(
            !cancelled.is_empty(),
            "cancel must always fire on touch move, even when place is skipped"
        );

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        echoer.abort();
        mover.abort();
    }

    /// PR-D10: seen_open=false (HL に届いていない or reject) の場合は
    /// `prev_in_flight_sz` を差し引かない。これは PR-D3 の reject_timeout 路線
    /// に委ねる設計。本テストは互換性: seen_open=false で partial fill が
    /// 起きないシナリオでは、新 place の sz が `remaining` のまま出ること。
    #[tokio::test(start_paused = true)]
    async fn passive_follow_race_cap_inactive_when_prev_quote_not_seen_open() {
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

        // Mover: 200ms で touch を動かす。
        let mover_state = state.clone();
        let mover_symbol = symbol.clone();
        let mover = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            set_book(&mover_state, &mover_symbol, dec!(2001), dec!(2000.5)).await;
        });

        // Watcher: 2 番目の cloid だけ full fill。echo back せず seen_open=false。
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
        // seen_open=false で fill が来ない c1 は race cap の対象外、
        // c2 が `remaining` 全量で出て full fill する従来挙動。
        assert!(!report.aborted, "abort_reason = {:?}", report.abort_reason);
        assert_eq!(report.filled_size, dec!(0.5));
        let placed: Vec<_> = mock.placed_calls().into_iter().flatten().collect();
        assert!(placed.len() >= 2);
        // c2 は remaining (= 0.5) サイズで出た (= cap 不発動)。
        assert_eq!(placed[1].sz, dec!(0.5));

        let _ = handle.shutdown.send(());
        let _ = handle.join.await;
        watcher.abort();
        mover.abort();
    }
}
