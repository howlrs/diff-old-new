//! WS state manager: applies WS-driven updates into `AppState`.
//!
//! The networked WS subscriber is added in PR-7 (server). This module exposes
//! a pure update function so unit tests can drive state without sockets.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use executor_core::cloid::Cloid;
use executor_core::state::{AppState, BookLevel, OpenOrder, OrderBook};
use executor_core::symbol::Symbol;
use executor_core::types::{Fill, OrderId, Side, Tif};

/// WS message types we accept (subset).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "camelCase")]
pub enum WsMessage {
    L2Book {
        coin: Symbol,
        bids: Vec<(Decimal, Decimal, u32)>,
        asks: Vec<(Decimal, Decimal, u32)>,
    },
    UserFill(WsFill),
    UserPosition(WsPosition),
    OrderUpdate(WsOrderUpdate),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsFill {
    pub coin: Symbol,
    pub cloid: Option<Cloid>,
    pub oid: u64,
    /// HL `tid` (trade id). Required for dedup between WS and REST polling
    /// fallback — partial fills against the same `oid` produce multiple
    /// distinct `tid`s, so `cloid` alone is insufficient (Gemini deep S1).
    pub tid: u64,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub fee: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsPosition {
    pub coin: Symbol,
    pub size: Decimal,
    pub entry_px: Option<Decimal>,
    pub margin_used: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsOrderUpdate {
    pub coin: Symbol,
    pub cloid: Option<Cloid>,
    pub oid: u64,
    pub status: WsOrderStatus,
    pub remaining_sz: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WsOrderStatus {
    Open,
    Filled,
    PartiallyFilled,
    Cancelled,
    Rejected,
}

#[derive(Debug)]
pub struct WsStateManager {
    state: Arc<AppState>,
    /// Dedup set for fills observed from BOTH the WS feed AND the REST polling
    /// fallback. Key is `(oid, tid)` because partial fills against the same
    /// order produce multiple distinct trade ids. Cloid alone is insufficient.
    /// Gemini deep S1 (2026-05-05): cloid-based dedup is a critical bug.
    seen_trade_ids: RwLock<HashSet<(OrderId, u64)>>,
}

impl WsStateManager {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            seen_trade_ids: RwLock::new(HashSet::new()),
        }
    }

    /// Apply a single WS message. All updates are short critical sections.
    pub async fn apply(&self, msg: WsMessage) {
        match msg {
            WsMessage::L2Book { coin, bids, asks } => self.apply_book(coin, bids, asks).await,
            WsMessage::UserFill(f) => self.apply_fill(f).await,
            WsMessage::UserPosition(p) => self.apply_position(p).await,
            WsMessage::OrderUpdate(u) => self.apply_order_update(u).await,
        }
        if let Ok(mut h) = self.state.health.try_write() {
            h.last_user_event = Some(Utc::now());
            h.ws_message_count = h.ws_message_count.saturating_add(1);
        }
    }

    async fn apply_book(
        &self,
        coin: Symbol,
        bids: Vec<(Decimal, Decimal, u32)>,
        asks: Vec<(Decimal, Decimal, u32)>,
    ) {
        let book = OrderBook {
            bids: bids
                .into_iter()
                .map(|(px, sz, n)| BookLevel { px, sz, n })
                .collect(),
            asks: asks
                .into_iter()
                .map(|(px, sz, n)| BookLevel { px, sz, n })
                .collect(),
            ts: Some(Utc::now()),
        };
        let mut g = self.state.book.write().await;
        g.insert(coin, book);
        if let Ok(mut h) = self.state.health.try_write() {
            h.last_book_update = Some(Utc::now());
        }
    }

    async fn apply_fill(&self, f: WsFill) {
        // PR-D1 Gemini deep S1: dedup BEFORE writing. WS and REST fallback
        // can both deliver the same trade. (oid, tid) is the unique key —
        // cloid alone misses partial fills.
        {
            let mut seen = self.seen_trade_ids.write().await;
            if !seen.insert((OrderId(f.oid), f.tid)) {
                tracing::trace!(
                    oid = f.oid,
                    tid = f.tid,
                    "ws_state: duplicate fill, ignoring"
                );
                return;
            }
        }
        let fill = Fill {
            symbol: f.coin.clone(),
            cloid: f.cloid,
            oid: OrderId(f.oid),
            side: f.side,
            px: f.px,
            sz: f.sz,
            fee: f.fee,
            ts: Utc::now(),
        };
        {
            let mut fills = self.state.recent_fills.write().await;
            fills.push_back(fill);
            const MAX_FILLS: usize = 1024;
            while fills.len() > MAX_FILLS {
                fills.pop_front();
            }
            // Drop the recent_fills lock before taking open_orders. Holding
            // both locks across an .await would serialise apply_fill behind
            // any reader of recent_fills (algorithms drain via `state.recent_fills`)
            // and adds an avoidable lock-order edge that could deadlock once
            // future code grabs them in the opposite order. Gemini review
            // 2026-05-05 flagged this when the explicit `drop(fills)` from
            // the PR-D4 path was naively walked back in PR-D8.
        }
        // remove or shrink open_orders entry by cloid
        if let Some(cloid) = f.cloid {
            let mut open = self.state.open_orders.write().await;
            if let Some(o) = open.get_mut(&cloid) {
                o.filled_sz = (o.filled_sz + f.sz).min(o.sz);
                if o.is_fully_filled() {
                    open.remove(&cloid);
                }
            }
        }
        // PR-D8 (2026-05-05): the PR-D4 path that extrapolated `f.sz/f.side`
        // into `state.position` here was reverted. Live observation showed
        // it double-counted: the WS userFills snapshot delivered just after
        // (re)connect re-applies historical fills already accounted for in
        // the preceding REST reconcile, so master ETH 0.105 reported as 0.210.
        //
        // `seen_trade_ids` only dedupes against fills WE saw earlier on this
        // process; it doesn't know what the REST snapshot already absorbed.
        // Without an HL-side `isSnapshot` flag (none exists) and given clock
        // drift makes timestamp gating unreliable, the safe answer is to
        // keep `state.position` strictly synced from the authoritative
        // sources (REST reconcile + future WsPosition / webData2 channel).
        //
        // Visibility gap: `/v1/positions` is updated at startup and every
        // 5 min reconcile. `passive_follow` derives size from `target_size`
        // absolutely (Intent::Open), so the gap doesn't affect it. A real
        // PR-D9 will subscribe HL `webData2` so the position channel is
        // pushed in real time as the authoritative absolute value.
    }

    async fn apply_position(&self, p: WsPosition) {
        let mut g = self.state.position.write().await;
        let cur = g.entry(p.coin).or_default();
        cur.size = p.size;
        cur.entry_px = p.entry_px;
        cur.margin_used = p.margin_used;
        cur.unrealized_pnl = p.unrealized_pnl;
        cur.last_update = Some(Utc::now());
    }

    async fn apply_order_update(&self, u: WsOrderUpdate) {
        let Some(cloid) = u.cloid else {
            return; // 必要なら oid ベースで管理
        };
        let mut open = self.state.open_orders.write().await;
        match u.status {
            WsOrderStatus::Cancelled | WsOrderStatus::Rejected | WsOrderStatus::Filled => {
                open.remove(&cloid);
            }
            WsOrderStatus::PartiallyFilled => {
                if let Some(o) = open.get_mut(&cloid) {
                    if let Some(rem) = u.remaining_sz {
                        o.filled_sz = (o.sz - rem).max(Decimal::ZERO);
                    }
                }
            }
            WsOrderStatus::Open => {
                if let Some(o) = open.get_mut(&cloid) {
                    o.oid = Some(OrderId(u.oid));
                }
            }
        }
    }

    /// Register an order we just sent (so subsequent fill events can update).
    pub async fn register_open_order(&self, order: OpenOrder) {
        let mut g = self.state.open_orders.write().await;
        g.insert(order.cloid, order);
    }

    /// PR-D1 reconcile: overwrite `AppState.position` and `AppState.open_orders`
    /// from a fresh REST snapshot.
    ///
    /// `recent_fills` is intentionally not touched (it's append-only history,
    /// dedup'd by (oid,tid) — REST polling fallback handles fill recovery).
    /// `seen_trade_ids` is also untouched: a reconnect doesn't invalidate
    /// dedup state.
    pub async fn reconcile(
        &self,
        open_orders_snapshot: Vec<crate::hl_client::HlOpenOrder>,
        account_snapshot: crate::hl_client::AccountStateSnapshot,
    ) {
        // Replace positions wholesale.
        {
            let mut g = self.state.position.write().await;
            *g = account_snapshot.positions.clone();
        }
        // Replace open_orders, but preserve any cloid we recorded locally
        // (HL's openOrders endpoint doesn't echo cloid, so an in-memory cloid
        // that hasn't yet been ack'd via WS would be lost otherwise).
        {
            let mut g = self.state.open_orders.write().await;
            // Build a temporary index of existing cloid → oid so we can match.
            let mut by_oid: std::collections::HashMap<u64, Cloid> =
                std::collections::HashMap::new();
            for (cloid, oo) in g.iter() {
                if let Some(OrderId(oid)) = oo.oid {
                    by_oid.insert(oid, *cloid);
                }
            }
            // Build the new map from REST snapshot.
            let mut new_map: std::collections::HashMap<Cloid, OpenOrder> =
                std::collections::HashMap::new();
            for o in &open_orders_snapshot {
                let cloid = by_oid.get(&o.oid.0).copied().unwrap_or_else(Cloid::new);
                new_map.insert(
                    cloid,
                    OpenOrder {
                        cloid,
                        oid: Some(o.oid),
                        symbol: o.symbol.clone(),
                        side: o.side,
                        px: o.limit_px,
                        sz: o.sz,
                        filled_sz: Decimal::ZERO,
                        tif: Tif::Gtc,
                        reduce_only: false,
                        placed_at: o.timestamp,
                    },
                );
            }
            *g = new_map;
        }
        // health.last_reconciliation
        if let Ok(mut h) = self.state.health.try_write() {
            h.last_reconciliation = Some(Utc::now());
        }
    }
}

/// Convenience: Build an OpenOrder from an OrderIntent + placed timestamp.
pub fn open_order_from_intent(
    intent: &executor_core::intent::OrderIntent,
    oid: Option<OrderId>,
) -> OpenOrder {
    OpenOrder {
        cloid: intent.cloid,
        oid,
        symbol: intent.symbol.clone(),
        side: intent.side,
        px: intent.px,
        sz: intent.sz,
        filled_sz: Decimal::ZERO,
        tif: intent.tif,
        reduce_only: intent.reduce_only,
        placed_at: Utc::now(),
    }
}

// Tif は Sized 確認用 (workspace lint)
#[allow(dead_code)]
fn _check_tif_is_sized() -> Tif {
    Tif::Gtc
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use executor_core::intent::OrderIntent;
    use executor_core::state::AppState;
    use rust_decimal_macros::dec;

    fn intent(cloid: Cloid) -> OrderIntent {
        OrderIntent {
            cloid,
            symbol: Symbol::new("BTC"),
            side: Side::Long,
            px: dec!(100),
            sz: dec!(1),
            tif: Tif::Gtc,
            reduce_only: false,
        }
    }

    #[tokio::test]
    async fn apply_book_writes_state() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        mgr.apply(WsMessage::L2Book {
            coin: Symbol::new("BTC"),
            bids: vec![(dec!(99), dec!(1), 1)],
            asks: vec![(dec!(101), dec!(1), 1)],
        })
        .await;
        let books = state.book.read().await;
        let b = books.get(&Symbol::new("BTC")).unwrap();
        assert_eq!(b.best_bid(), Some(dec!(99)));
        assert_eq!(b.best_ask(), Some(dec!(101)));
    }

    #[tokio::test]
    async fn apply_fill_partial_then_full_removes_open_order() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        let cloid = Cloid::new();
        mgr.register_open_order(open_order_from_intent(&intent(cloid), None))
            .await;

        // partial
        mgr.apply(WsMessage::UserFill(WsFill {
            coin: Symbol::new("BTC"),
            cloid: Some(cloid),
            oid: 1,
            tid: 1001,
            side: Side::Long,
            px: dec!(100),
            sz: dec!(0.4),
            fee: dec!(0.001),
        }))
        .await;
        {
            let open = state.open_orders.read().await;
            assert!(open.contains_key(&cloid));
            assert_eq!(open.get(&cloid).unwrap().filled_sz, dec!(0.4));
        }

        // remaining filled → removed
        mgr.apply(WsMessage::UserFill(WsFill {
            coin: Symbol::new("BTC"),
            cloid: Some(cloid),
            oid: 1,
            tid: 1002,
            side: Side::Long,
            px: dec!(100),
            sz: dec!(0.6),
            fee: dec!(0.001),
        }))
        .await;
        let open = state.open_orders.read().await;
        assert!(!open.contains_key(&cloid));
    }

    #[tokio::test]
    async fn apply_position_updates_state() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        mgr.apply(WsMessage::UserPosition(WsPosition {
            coin: Symbol::new("BTC"),
            size: dec!(2.5),
            entry_px: Some(dec!(50000)),
            margin_used: Some(dec!(1000)),
            unrealized_pnl: Some(dec!(0)),
        }))
        .await;
        let pos = state.position.read().await;
        let p = pos.get(&Symbol::new("BTC")).unwrap();
        assert_eq!(p.size, dec!(2.5));
    }

    #[tokio::test]
    async fn apply_fill_dedup_skips_duplicate_oid_tid() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        let cloid = Cloid::new();
        mgr.register_open_order(open_order_from_intent(&intent(cloid), None))
            .await;
        let f = WsFill {
            coin: Symbol::new("BTC"),
            cloid: Some(cloid),
            oid: 7,
            tid: 9001,
            side: Side::Long,
            px: dec!(50000),
            sz: dec!(0.3),
            fee: dec!(0.001),
        };
        mgr.apply(WsMessage::UserFill(f.clone())).await;
        // Same (oid, tid) — must be ignored, recent_fills count unchanged.
        mgr.apply(WsMessage::UserFill(f.clone())).await;
        // Read + drop in a scope so the next `apply` call can acquire the
        // write lock. Holding `read()` across an `await` that itself takes
        // `write()` deadlocks tokio's RwLock.
        {
            let fills = state.recent_fills.read().await;
            assert_eq!(fills.len(), 1, "duplicate (oid,tid) must dedup");
        }

        // Different tid — must be applied.
        let f2 = WsFill { tid: 9002, ..f };
        mgr.apply(WsMessage::UserFill(f2)).await;
        let fills = state.recent_fills.read().await;
        assert_eq!(fills.len(), 2, "distinct tid must record");
    }

    #[tokio::test]
    async fn apply_order_update_cancelled_removes() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        let cloid = Cloid::new();
        mgr.register_open_order(open_order_from_intent(&intent(cloid), None))
            .await;
        mgr.apply(WsMessage::OrderUpdate(WsOrderUpdate {
            coin: Symbol::new("BTC"),
            cloid: Some(cloid),
            oid: 1,
            status: WsOrderStatus::Cancelled,
            remaining_sz: None,
        }))
        .await;
        let open = state.open_orders.read().await;
        assert!(!open.contains_key(&cloid));
    }

    /// PR-D8 regression: a userFills WS message must NOT mutate
    /// `state.position`. The only writers are `apply_position` (WS
    /// userPosition / future webData2) and `reconcile` (REST snapshot).
    /// Live observation 2026-05-05: PR-D4 had apply_fill +=size which
    /// double-counted against the WS reconnect snapshot, ETH 0.105 → 0.210.
    #[tokio::test]
    async fn apply_fill_does_not_touch_position() {
        let state = Arc::new(AppState::new());
        let mgr = WsStateManager::new(state.clone());
        // Pre-condition: a reconcile-style position is in place.
        {
            let mut g = state.position.write().await;
            g.insert(
                Symbol::new("ETH"),
                executor_core::state::Position {
                    size: dec!(0.105),
                    ..Default::default()
                },
            );
        }
        mgr.apply(WsMessage::UserFill(WsFill {
            coin: Symbol::new("ETH"),
            cloid: None,
            oid: 1,
            tid: 1,
            side: Side::Long,
            px: dec!(2400),
            sz: dec!(0.005),
            fee: dec!(0.001),
        }))
        .await;
        let pos = state.position.read().await;
        let p = pos.get(&Symbol::new("ETH")).unwrap();
        assert_eq!(
            p.size,
            dec!(0.105),
            "apply_fill must not extrapolate into position; that responsibility \
             belongs to apply_position / reconcile only"
        );
    }
}
