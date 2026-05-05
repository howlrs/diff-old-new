//! Hyperliquid HTTP client (mock + real interface).
//!
//! 80 % プロト範囲 では `MockHlClient` を使う. `RealHlClient` は構造だけ用意し
//! 内部の EIP-712 sign + `/exchange` POST は次フェーズで実装 (鍵管理 ブレスト後).

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use executor_core::cloid::Cloid;
use executor_core::intent::{CancelIntent, OrderIntent};
use executor_core::state::{OrderBook, Position};
use executor_core::symbol::Symbol;
use executor_core::types::{Address, OrderId};

use crate::errors::HlError;
use crate::rate_limiter::TokenBucket;
use crate::signer::Signer;

/// Endpoint configuration (mainnet / testnet / custom).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlConfig {
    pub info_url: String,
    pub exchange_url: String,
    pub ws_url: String,
}

impl HlConfig {
    pub fn mainnet() -> Self {
        Self {
            info_url: "https://api.hyperliquid.xyz/info".into(),
            exchange_url: "https://api.hyperliquid.xyz/exchange".into(),
            ws_url: "wss://api.hyperliquid.xyz/ws".into(),
        }
    }

    pub fn testnet() -> Self {
        Self {
            info_url: "https://api.hyperliquid-testnet.xyz/info".into(),
            exchange_url: "https://api.hyperliquid-testnet.xyz/exchange".into(),
            ws_url: "wss://api.hyperliquid-testnet.xyz/ws".into(),
        }
    }
}

/// Account-level snapshot returned by `/info clearinghouseState`.
///
/// HL returns numeric values as JSON strings; this struct holds them as
/// `Decimal` after `from_wire` mapping. `address` is the master EOA the
/// snapshot was fetched for; `server_time` is HL's snapshot timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStateSnapshot {
    pub address: Address,
    pub margin_used: Decimal,
    pub account_value: Decimal,
    pub withdrawable: Decimal,
    pub cross_maintenance_margin_used: Decimal,
    pub positions: HashMap<Symbol, Position>,
    pub open_orders_by_cloid: HashMap<Cloid, OrderId>,
    pub fetched_at: DateTime<Utc>,
    pub server_time: DateTime<Utc>,
}

impl AccountStateSnapshot {
    /// Map the wire representation into the domain snapshot.
    pub fn from_wire(address: Address, wire: &crate::wire::WireClearinghouseState) -> Self {
        let now = Utc::now();
        let server_time = chrono::Utc
            .timestamp_millis_opt(wire.time as i64)
            .single()
            .unwrap_or(now);
        let mut positions = HashMap::new();
        for ap in &wire.asset_positions {
            let p = &ap.position;
            positions.insert(
                Symbol::new(&p.coin),
                Position {
                    size: p.szi,
                    entry_px: Some(p.entry_px),
                    unrealized_pnl: Some(p.unrealized_pnl),
                    margin_used: Some(p.margin_used),
                    last_update: Some(server_time),
                },
            );
        }
        Self {
            address,
            margin_used: wire.margin_summary.total_margin_used,
            account_value: wire.margin_summary.account_value,
            withdrawable: wire.withdrawable,
            cross_maintenance_margin_used: wire.cross_maintenance_margin_used,
            positions,
            open_orders_by_cloid: HashMap::new(),
            fetched_at: now,
            server_time,
        }
    }

    /// Empty snapshot for tests/mocks where no real data is needed yet.
    pub fn empty(address: Address) -> Self {
        let now = Utc::now();
        Self {
            address,
            margin_used: Decimal::ZERO,
            account_value: Decimal::ZERO,
            withdrawable: Decimal::ZERO,
            cross_maintenance_margin_used: Decimal::ZERO,
            positions: HashMap::new(),
            open_orders_by_cloid: HashMap::new(),
            fetched_at: now,
            server_time: now,
        }
    }
}

/// Place-order response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    pub cloid: Cloid,
    pub oid: Option<OrderId>,
    pub status: String,
    pub error: Option<String>,
}

#[async_trait]
pub trait HlClient: Send + Sync {
    /// Fetch full account state (positions, open orders, margin).
    async fn fetch_account_state(&self, address: &Address)
        -> Result<AccountStateSnapshot, HlError>;

    /// Fetch a fresh top-N book snapshot (for state init / reconciliation).
    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError>;

    /// Place a batch of orders. Returns per-cloid response.
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError>;

    /// Cancel a batch of orders by cloid (preferred) or oid.
    async fn cancel_orders(&self, cancels: &[CancelIntent]) -> Result<Vec<OrderResponse>, HlError>;
}

/// `MockHlClient` records calls in memory and returns Ok responses. Used for
/// the 80 % prototype, unit tests, and integration tests with no key.
#[derive(Debug)]
pub struct MockHlClient {
    placed: Mutex<Vec<Vec<OrderIntent>>>,
    cancelled: Mutex<Vec<Vec<CancelIntent>>>,
    /// Pre-seeded account state (so tests can simulate "current position").
    pub account: Mutex<AccountStateSnapshot>,
    /// Pre-seeded book per symbol.
    pub books: Mutex<HashMap<Symbol, OrderBook>>,
}

impl Default for MockHlClient {
    fn default() -> Self {
        Self {
            placed: Mutex::new(Vec::new()),
            cancelled: Mutex::new(Vec::new()),
            account: Mutex::new(AccountStateSnapshot::empty(Address::new(
                "0x0000000000000000000000000000000000000000",
            ))),
            books: Mutex::new(HashMap::new()),
        }
    }
}

impl MockHlClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn placed_calls(&self) -> Vec<Vec<OrderIntent>> {
        self.placed.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn cancelled_calls(&self) -> Vec<Vec<CancelIntent>> {
        self.cancelled.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn seed_book(&self, symbol: Symbol, book: OrderBook) {
        if let Ok(mut g) = self.books.lock() {
            g.insert(symbol, book);
        }
    }

    pub fn seed_account(&self, snap: AccountStateSnapshot) {
        if let Ok(mut g) = self.account.lock() {
            *g = snap;
        }
    }
}

#[async_trait]
impl HlClient for MockHlClient {
    async fn fetch_account_state(
        &self,
        address: &Address,
    ) -> Result<AccountStateSnapshot, HlError> {
        let snap = self
            .account
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| AccountStateSnapshot::empty(address.clone()));
        Ok(snap)
    }

    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError> {
        let books = self
            .books
            .lock()
            .map_err(|e| HlError::Network(format!("mock book lock: {e}")))?;
        books
            .get(symbol)
            .cloned()
            .ok_or_else(|| HlError::InvalidResponse(format!("no seeded book for {symbol}")))
    }

    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if let Ok(mut g) = self.placed.lock() {
            g.push(orders.to_vec());
        }
        // すべて成功扱い
        let resp = orders
            .iter()
            .map(|o| OrderResponse {
                cloid: o.cloid,
                oid: Some(OrderId(rand_oid())),
                status: "ok".into(),
                error: None,
            })
            .collect();
        Ok(resp)
    }

    async fn cancel_orders(&self, cancels: &[CancelIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if let Ok(mut g) = self.cancelled.lock() {
            g.push(cancels.to_vec());
        }
        let resp = cancels
            .iter()
            .map(|c| OrderResponse {
                cloid: c.by_cloid.unwrap_or_default(),
                oid: c.by_oid,
                status: "cancelled".into(),
                error: None,
            })
            .collect();
        Ok(resp)
    }
}

/// 擬似 oid (mock 用).
/// Gemini PR-2 review: nanos は u64 飽和に近いので micros 採用.
fn rand_oid() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_default()
}

/// Skeleton for the real client. Networking + sign は別 PR で完成させる.
pub struct RealHlClient {
    pub config: HlConfig,
    pub signer: Arc<dyn Signer>,
    pub rate_limiter: Arc<TokenBucket>,
    pub http: reqwest::Client,
}

impl RealHlClient {
    pub fn new(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Some(std::time::Duration::from_secs(60)))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            config,
            signer,
            rate_limiter: Arc::new(TokenBucket::hyperliquid_default()),
            http,
        }
    }
}

#[async_trait]
impl HlClient for RealHlClient {
    async fn fetch_account_state(
        &self,
        address: &Address,
    ) -> Result<AccountStateSnapshot, HlError> {
        let _wait = self.rate_limiter.acquire(2).await;
        let body = serde_json::json!({
            "type": "clearinghouseState",
            "user": address.as_str(),
        });
        let resp = self
            .http
            .post(&self.config.info_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(HlError::Network(format!("HTTP {}", resp.status())));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        let wire: crate::wire::WireClearinghouseState = serde_json::from_str(&text)
            .map_err(|e| HlError::InvalidResponse(format!("clearinghouseState: {e}")))?;
        Ok(AccountStateSnapshot::from_wire(address.clone(), &wire))
    }

    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError> {
        let _wait = self.rate_limiter.acquire(1).await;
        let body = serde_json::json!({
            "type": "l2Book",
            "coin": symbol.as_str(),
        });
        let resp = self
            .http
            .post(&self.config.info_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(HlError::Network(format!("HTTP {}", resp.status())));
        }
        // 80% プロト: 空 book を返す. 実装は次 PR で詳細化.
        Ok(OrderBook::default())
    }

    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        // 80% プロト: signer + rate_limiter の枠だけ通す. 実 POST は鍵管理ブレスト後.
        if orders.is_empty() {
            return Ok(vec![]);
        }
        let _wait = self.rate_limiter.acquire(orders.len() as u32).await;

        // sign for nonce visibility (no real submission).
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let action = serde_json::json!({"type": "order", "orders": orders.len()});
        let _sig = self.signer.sign_l1(&action, nonce).await?;

        Err(HlError::Exchange {
            code: Some("not_implemented".into()),
            message: "RealHlClient::place_orders is the 80% scaffold; \
                      full HL signing arrives with the key-management PR"
                .into(),
        })
    }

    async fn cancel_orders(&self, cancels: &[CancelIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if cancels.is_empty() {
            return Ok(vec![]);
        }
        let _wait = self.rate_limiter.acquire(cancels.len() as u32).await;
        Err(HlError::Exchange {
            code: Some("not_implemented".into()),
            message: "RealHlClient::cancel_orders scaffold (see place_orders).".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use executor_core::types::{Side, Tif};
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn mock_records_orders_and_cancels() {
        let m = MockHlClient::new();
        let oi = OrderIntent {
            cloid: Cloid::new(),
            symbol: Symbol::new("BTC"),
            side: Side::Long,
            px: dec!(50000),
            sz: dec!(0.1),
            tif: Tif::Gtc,
            reduce_only: false,
        };
        let resp = m.place_orders(std::slice::from_ref(&oi)).await.unwrap();
        assert_eq!(resp.len(), 1);
        assert_eq!(resp[0].cloid, oi.cloid);
        assert_eq!(m.placed_calls().len(), 1);
        let ci = CancelIntent {
            symbol: Symbol::new("BTC"),
            by_cloid: Some(oi.cloid),
            by_oid: None,
        };
        let resp = m.cancel_orders(&[ci]).await.unwrap();
        assert_eq!(resp.len(), 1);
        assert_eq!(resp[0].status, "cancelled");
    }

    #[tokio::test]
    async fn mock_fetch_book_returns_seeded() {
        let m = MockHlClient::new();
        m.seed_book(Symbol::new("ETH"), OrderBook::default());
        let book = m.fetch_book_snapshot(&Symbol::new("ETH")).await.unwrap();
        assert!(book.bids.is_empty());
    }

    #[tokio::test]
    async fn mock_fetch_book_missing_returns_err() {
        let m = MockHlClient::new();
        let err = m
            .fetch_book_snapshot(&Symbol::new("XXX"))
            .await
            .unwrap_err();
        assert!(matches!(err, HlError::InvalidResponse(_)));
    }

    #[test]
    fn hl_config_endpoints() {
        let m = HlConfig::mainnet();
        assert!(m.info_url.contains("api.hyperliquid.xyz"));
        let t = HlConfig::testnet();
        assert!(t.info_url.contains("testnet"));
    }
}
