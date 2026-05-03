//! Shared application state with split locks (Gemini v2 review reflection).
//!
//! A single `Arc<RwLock<LocalState>>` would let high-frequency book updates
//! (write) starve algorithm reads (read). We split per concern so updates to
//! one slice (e.g. book) never block readers of another (e.g. position).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::cloid::Cloid;
use crate::nonce::NonceManager;
use crate::symbol::Symbol;
use crate::types::{Fill, OrderId, Side, Tif};

/// Top-N orderbook level (px + total size + n orders).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub px: Decimal,
    pub sz: Decimal,
    pub n: u32,
}

/// Top-N levels per side (bids descending, asks ascending).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderBook {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub ts: Option<DateTime<Utc>>,
}

impl OrderBook {
    pub fn best_bid(&self) -> Option<Decimal> {
        self.bids.first().map(|l| l.px)
    }

    pub fn best_ask(&self) -> Option<Decimal> {
        self.asks.first().map(|l| l.px)
    }

    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / Decimal::from(2)),
            _ => None,
        }
    }

    pub fn spread_bps(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask(), self.mid()) {
            (Some(b), Some(a), Some(m)) if m > Decimal::ZERO => {
                Some((a - b) / m * Decimal::from(10000))
            }
            _ => None,
        }
    }
}

/// Position state for one symbol.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    pub size: Decimal, // signed (positive=long, negative=short)
    pub entry_px: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub margin_used: Option<Decimal>,
    pub last_update: Option<DateTime<Utc>>,
}

impl Position {
    pub fn side(&self) -> Option<Side> {
        if self.size > Decimal::ZERO {
            Some(Side::Long)
        } else if self.size < Decimal::ZERO {
            Some(Side::Short)
        } else {
            None
        }
    }
}

/// One open order tracked locally (registered on place success).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenOrder {
    pub cloid: Cloid,
    pub oid: Option<OrderId>,
    pub symbol: Symbol,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub filled_sz: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
    pub placed_at: DateTime<Utc>,
}

impl OpenOrder {
    pub fn remaining_sz(&self) -> Decimal {
        (self.sz - self.filled_sz).max(Decimal::ZERO)
    }

    pub fn is_fully_filled(&self) -> bool {
        self.filled_sz >= self.sz
    }
}

/// Connection / freshness diagnostics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthStatus {
    pub ws_connected: bool,
    pub last_book_update: Option<DateTime<Utc>>,
    pub last_user_event: Option<DateTime<Utc>>,
    pub last_reconciliation: Option<DateTime<Utc>>,
    pub ws_reconnect_count: u32,
    pub ws_message_count: u64,
}

/// Top-level shared state. Each field is independently lockable.
///
/// Algorithms hold an `Arc<AppState>` and lock only the slice they need.
#[derive(Debug)]
pub struct AppState {
    pub book: Arc<RwLock<HashMap<Symbol, OrderBook>>>,
    pub position: Arc<RwLock<HashMap<Symbol, Position>>>,
    pub open_orders: Arc<RwLock<HashMap<Cloid, OpenOrder>>>,
    pub recent_fills: Arc<RwLock<VecDeque<Fill>>>,
    pub health: Arc<RwLock<HealthStatus>>,
    pub nonce_mgr: Arc<NonceManager>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            book: Arc::new(RwLock::new(HashMap::new())),
            position: Arc::new(RwLock::new(HashMap::new())),
            open_orders: Arc::new(RwLock::new(HashMap::new())),
            recent_fills: Arc::new(RwLock::new(VecDeque::with_capacity(1024))),
            health: Arc::new(RwLock::new(HealthStatus::default())),
            nonce_mgr: Arc::new(NonceManager::new()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use rust_decimal_macros::dec;

    fn lvl(px: Decimal, sz: Decimal) -> BookLevel {
        BookLevel { px, sz, n: 1 }
    }

    #[test]
    fn book_best_quotes() {
        let book = OrderBook {
            bids: vec![lvl(dec!(99.0), dec!(1.0)), lvl(dec!(98.5), dec!(2.0))],
            asks: vec![lvl(dec!(100.0), dec!(1.0)), lvl(dec!(100.5), dec!(2.0))],
            ts: None,
        };
        assert_eq!(book.best_bid(), Some(dec!(99.0)));
        assert_eq!(book.best_ask(), Some(dec!(100.0)));
        assert_eq!(book.mid(), Some(dec!(99.5)));
    }

    #[test]
    fn book_spread_bps() {
        let book = OrderBook {
            bids: vec![lvl(dec!(100.0), dec!(1.0))],
            asks: vec![lvl(dec!(100.1), dec!(1.0))],
            ts: None,
        };
        // spread = 0.1 / 100.05 * 10000 ≈ 9.995 bps
        let bps = book.spread_bps().unwrap();
        assert!((bps - dec!(9.995)).abs() < dec!(0.01));
    }

    #[test]
    fn position_side() {
        let mut p = Position::default();
        assert_eq!(p.side(), None);
        p.size = dec!(10);
        assert_eq!(p.side(), Some(Side::Long));
        p.size = dec!(-5);
        assert_eq!(p.side(), Some(Side::Short));
    }

    #[test]
    fn open_order_remaining() {
        let o = OpenOrder {
            cloid: Cloid::new(),
            oid: None,
            symbol: Symbol::new("BTC"),
            side: Side::Long,
            px: dec!(50000),
            sz: dec!(1.0),
            filled_sz: dec!(0.3),
            tif: Tif::Gtc,
            reduce_only: false,
            placed_at: Utc::now(),
        };
        assert_eq!(o.remaining_sz(), dec!(0.7));
        assert!(!o.is_fully_filled());
    }

    #[tokio::test]
    async fn app_state_split_locks_dont_block_each_other() {
        let s = AppState::new();
        // hold a read on book, then take a write on position simultaneously.
        let _book_read = s.book.read().await;
        let mut pos = s.position.write().await;
        pos.insert(Symbol::new("BTC"), Position::default());
        // 何もブロックされず到達できれば OK (timeoutで死なない)
    }
}
