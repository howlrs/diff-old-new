//! WS state manager: applies WS-driven updates into `AppState`.
//!
//! The networked WS subscriber is added in PR-7 (server). This module exposes
//! a pure update function so unit tests can drive state without sockets.

use std::sync::Arc;

use chrono::Utc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone)]
pub struct WsStateManager {
    state: Arc<AppState>,
}

impl WsStateManager {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
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
        let mut fills = self.state.recent_fills.write().await;
        fills.push_back(fill);
        const MAX_FILLS: usize = 1024;
        while fills.len() > MAX_FILLS {
            fills.pop_front();
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
}
