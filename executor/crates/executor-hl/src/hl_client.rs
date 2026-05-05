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

/// Domain-level open order (one HL `openOrders` entry mapped to local types).
///
/// We keep the wire `oid` and translate side into `executor_core::types::Side`
/// for callers; cloid is unknown from `openOrders` alone (HL does not echo it
/// in this endpoint), so it stays None until matched with local registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlOpenOrder {
    pub symbol: Symbol,
    pub side: executor_core::types::Side,
    pub limit_px: Decimal,
    pub sz: Decimal,
    pub oid: OrderId,
    pub timestamp: DateTime<Utc>,
}

impl HlOpenOrder {
    pub fn from_wire(w: &crate::wire::WireOpenOrder) -> Self {
        use crate::wire::WireOrderSide;
        let side = match w.side {
            WireOrderSide::A => executor_core::types::Side::Short, // ask = sell
            WireOrderSide::B => executor_core::types::Side::Long,  // bid = buy
        };
        let ts = chrono::Utc
            .timestamp_millis_opt(w.timestamp)
            .single()
            .unwrap_or_else(Utc::now);
        Self {
            symbol: Symbol::new(&w.coin),
            side,
            limit_px: w.limit_px,
            sz: w.sz,
            oid: OrderId(w.oid),
            timestamp: ts,
        }
    }
}

/// HL `userRole` mapped to a Rust enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Agent { master: Address },
    Vault,
    SubAccount,
    Missing,
}

/// Catch-all for HL roles we don't know yet (forward compatibility).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownRole(pub String);

impl Role {
    pub fn from_wire(w: &crate::wire::WireUserRole) -> Self {
        use crate::wire::{WireUserRole as W, WireUserRoleTagged as T};
        match w {
            W::Tagged(t) => match t {
                T::User => Role::User,
                T::Agent { data } => Role::Agent {
                    master: Address::new(&data.user),
                },
                T::Vault => Role::Vault,
                T::SubAccount => Role::SubAccount,
                T::Missing => Role::Missing,
            },
            // Unknown JSON value (e.g. `{"role":"marketMaker"}`) — preserve it
            // as Missing so callers don't crash but behavior matches the
            // safest known variant.
            W::Unknown(_) => Role::Missing,
        }
    }
}

#[async_trait]
pub trait HlClient: Send + Sync {
    /// Fetch full account state. `dex=None` selects the default perp dex;
    /// `dex=Some("xyz")` etc. selects a HIP-3 builder dex.
    async fn fetch_account_state(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<AccountStateSnapshot, HlError>;

    /// Fetch a fresh top-N book snapshot.
    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError>;

    /// Fetch all open orders for the address (default dex unless specified).
    async fn fetch_open_orders(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<Vec<HlOpenOrder>, HlError>;

    /// Fetch the perp universe metadata (symbol list + leverage caps).
    async fn fetch_meta(&self, dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError>;

    /// Identify the role of an address (catches agent/master mix-ups).
    async fn fetch_user_role(&self, address: &Address) -> Result<Role, HlError>;

    /// Place a batch of orders.
    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError>;

    /// Cancel a batch of orders.
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
        _dex: Option<&str>,
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

    async fn fetch_open_orders(
        &self,
        _address: &Address,
        _dex: Option<&str>,
    ) -> Result<Vec<HlOpenOrder>, HlError> {
        Ok(Vec::new())
    }

    async fn fetch_meta(&self, _dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError> {
        Ok(crate::wire::WireMeta {
            universe: Vec::new(),
        })
    }

    async fn fetch_user_role(&self, _address: &Address) -> Result<Role, HlError> {
        Ok(Role::User)
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
    /// PR-C1: pre-built symbol → asset cache. `RealHlClient::bootstrap()`
    /// initializes this empty so `fetch_meta()` can be called to BUILD the
    /// cache; `with_meta()` upgrades it. After `with_meta`, this is the
    /// authoritative source for asset resolution in place/cancel.
    pub meta: Arc<crate::meta::MetaCache>,
}

impl RealHlClient {
    /// Construct a client with an EMPTY MetaCache. Use this when you need
    /// to call `fetch_meta()` to build the real cache; afterward, call
    /// `with_meta()` to produce a production-ready client.
    pub fn bootstrap(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
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
            meta: Arc::new(crate::meta::MetaCache::empty()),
        }
    }

    /// Replace the MetaCache. Returns a new `RealHlClient` reusing the
    /// existing http/signer/rate_limiter. Call this after building the cache.
    pub fn with_meta(self, meta: Arc<crate::meta::MetaCache>) -> Self {
        Self { meta, ..self }
    }

    /// Backwards-compatible alias. Existing callers used `new(config, signer)`
    /// with the implicit assumption that meta would be filled in later.
    /// New code should use `bootstrap` + `with_meta` explicitly.
    pub fn new(config: HlConfig, signer: Arc<dyn Signer>) -> Self {
        Self::bootstrap(config, signer)
    }

    /// Resolve a symbol to its asset index using the cached meta.
    /// Used by both `place_orders` and `cancel_orders` (Task 6).
    pub(crate) fn resolve_asset(
        &self,
        symbol: &executor_core::symbol::Symbol,
    ) -> Result<u32, HlError> {
        self.meta.resolve(symbol)
    }

    /// POST a JSON body to the /info endpoint and return the response body as a String.
    /// Maps non-2xx into `HlError::Network` with the response body included so
    /// HL's JSON error detail (e.g. "Unknown dex", rate-limit reason) is
    /// preserved for diagnosis.
    async fn post_info(&self, body: &serde_json::Value) -> Result<String, HlError> {
        let resp = self
            .http
            .post(&self.config.info_url)
            .json(body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(HlError::Network(format!("HTTP {status}: {text}")));
        }
        Ok(text)
    }

    /// POST a JSON body to the /exchange endpoint and return the response body.
    /// Mirrors `post_info` but targets `config.exchange_url`.
    async fn post_exchange(&self, body: &serde_json::Value) -> Result<String, HlError> {
        let resp = self
            .http
            .post(&self.config.exchange_url)
            .json(body)
            .send()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(HlError::Network(format!("HTTP {status}: {text}")));
        }
        Ok(text)
    }
}

#[async_trait]
impl HlClient for RealHlClient {
    async fn fetch_account_state(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<AccountStateSnapshot, HlError> {
        let _wait = self.rate_limiter.acquire(2).await;
        let mut body = serde_json::json!({
            "type": "clearinghouseState",
            "user": address.as_str(),
        });
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::WireClearinghouseState = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("clearinghouseState: {e}")))?;
        Ok(AccountStateSnapshot::from_wire(address.clone(), &wire))
    }

    async fn fetch_book_snapshot(&self, symbol: &Symbol) -> Result<OrderBook, HlError> {
        let _wait = self.rate_limiter.acquire(2).await;
        let body = serde_json::json!({
            "type": "l2Book",
            "coin": symbol.as_str(),
        });
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::WireL2Book = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("l2Book: {e}")))?;
        Ok(wire.to_orderbook())
    }

    async fn fetch_open_orders(
        &self,
        address: &Address,
        dex: Option<&str>,
    ) -> Result<Vec<HlOpenOrder>, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let mut body = serde_json::json!({
            "type": "openOrders",
            "user": address.as_str(),
        });
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        let wire: Vec<crate::wire::WireOpenOrder> = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("openOrders: {e}")))?;
        Ok(wire.iter().map(HlOpenOrder::from_wire).collect())
    }

    async fn fetch_meta(&self, dex: Option<&str>) -> Result<crate::wire::WireMeta, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let mut body = serde_json::json!({"type": "meta"});
        if let Some(d) = dex {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self.post_info(&body).await?;
        serde_json::from_str(&resp).map_err(|e| HlError::InvalidResponse(format!("meta: {e}")))
    }

    async fn fetch_user_role(&self, address: &Address) -> Result<Role, HlError> {
        let _wait = self.rate_limiter.acquire(20).await;
        let body = serde_json::json!({
            "type": "userRole",
            "user": address.as_str(),
        });
        let resp = self.post_info(&body).await?;
        let wire: crate::wire::WireUserRole = serde_json::from_str(&resp)
            .map_err(|e| HlError::InvalidResponse(format!("userRole: {e}")))?;
        Ok(Role::from_wire(&wire))
    }

    async fn place_orders(&self, orders: &[OrderIntent]) -> Result<Vec<OrderResponse>, HlError> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }

        let weight = 1 + (orders.len() as u32 / 40);
        let _wait = self.rate_limiter.acquire(weight).await;

        // Resolve each intent's asset; collect resolved (idx, wire) pairs and
        // immediately-rejected (idx, error_response) pairs so the final output
        // matches the input order exactly. UnknownSymbol drops a single order;
        // other Err variants abort the whole batch.
        let mut responses: Vec<Option<OrderResponse>> = (0..orders.len()).map(|_| None).collect();
        let mut wires_with_idx: Vec<(usize, crate::eip712::OrderWire)> =
            Vec::with_capacity(orders.len());
        for (i, intent) in orders.iter().enumerate() {
            match self.resolve_asset(&intent.symbol) {
                Ok(asset) => {
                    wires_with_idx
                        .push((i, crate::eip712::order_intent_to_wire(intent, asset)));
                }
                Err(HlError::UnknownSymbol(sym)) => {
                    tracing::error!(symbol = %sym, "place_orders: unknown symbol; dropping");
                    responses[i] = Some(OrderResponse {
                        cloid: intent.cloid,
                        oid: None,
                        status: "error".into(),
                        error: Some(format!("unknown symbol: {sym}")),
                    });
                }
                Err(e) => return Err(e),
            }
        }

        if wires_with_idx.is_empty() {
            return Ok(responses.into_iter().flatten().collect());
        }

        let order_wires: Vec<crate::eip712::OrderWire> =
            wires_with_idx.iter().map(|(_, w)| w.clone()).collect();
        let action = crate::eip712::OrderAction {
            action_type: "order".into(),
            orders: order_wires,
            grouping: "na".into(),
        };
        let action_value = serde_json::to_value(&action)
            .map_err(|e| HlError::ActionFormat(format!("order serialize: {e}")))?;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();

        let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

        let body = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": sig,
            "vaultAddress": serde_json::Value::Null,
        });

        let resp_text = self.post_exchange(&body).await?;

        // Parse only the orders that were actually sent. Build a Vec<OrderIntent>
        // matching wires_with_idx order so parse_exchange_response can attribute
        // each parsed status to the originating intent's cloid.
        let sent_intents_owned: Vec<OrderIntent> =
            wires_with_idx.iter().map(|(i, _)| orders[*i].clone()).collect();
        let parsed = parse_exchange_response(&resp_text, &sent_intents_owned)?;

        for ((i, _), resp) in wires_with_idx.iter().zip(parsed) {
            responses[*i] = Some(resp);
        }

        Ok(responses.into_iter().flatten().collect())
    }

    async fn cancel_orders(
        &self,
        cancels: &[CancelIntent],
    ) -> Result<Vec<OrderResponse>, HlError> {
        if cancels.is_empty() {
            return Ok(Vec::new());
        }

        let weight = 1 + (cancels.len() as u32 / 40);
        let _wait = self.rate_limiter.acquire(weight).await;

        // Validate by_cloid presence and resolve asset per intent. Same
        // index-preservation pattern as place_orders.
        let mut responses: Vec<Option<OrderResponse>> = (0..cancels.len()).map(|_| None).collect();
        let mut wires_with_idx: Vec<(usize, crate::eip712::CancelByCloidWire)> =
            Vec::with_capacity(cancels.len());
        for (i, c) in cancels.iter().enumerate() {
            // by_oid-only is still rejected (PR-B2a contract; PR-B2b
            // emergency_stop fix accepts by_cloid + by_oid, using cloid).
            let cloid = match c.by_cloid {
                Some(cl) => cl,
                None => {
                    return Err(HlError::ActionFormat(
                        "by_oid-only cancel not supported in PR-B2a; CancelIntent must \
                         include by_cloid (PR-B2b will add by_oid path)"
                            .into(),
                    ));
                }
            };
            match self.resolve_asset(&c.symbol) {
                Ok(asset) => {
                    wires_with_idx.push((
                        i,
                        crate::eip712::CancelByCloidWire {
                            asset,
                            cloid: format!("{cloid}"),
                        },
                    ));
                }
                Err(HlError::UnknownSymbol(sym)) => {
                    tracing::error!(symbol = %sym, "cancel_orders: unknown symbol; dropping");
                    responses[i] = Some(OrderResponse {
                        cloid,
                        oid: None,
                        status: "error".into(),
                        error: Some(format!("unknown symbol: {sym}")),
                    });
                }
                Err(e) => return Err(e),
            }
        }

        if wires_with_idx.is_empty() {
            return Ok(responses.into_iter().flatten().collect());
        }

        let cancel_wires: Vec<crate::eip712::CancelByCloidWire> =
            wires_with_idx.iter().map(|(_, w)| w.clone()).collect();
        let action = crate::eip712::CancelByCloidAction {
            action_type: "cancelByCloid".into(),
            cancels: cancel_wires,
        };
        let action_value = serde_json::to_value(&action)
            .map_err(|e| HlError::ActionFormat(format!("cancel serialize: {e}")))?;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();

        let sig = self.signer.sign_l1(&action_value, nonce, None).await?;

        let body = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": sig,
            "vaultAddress": serde_json::Value::Null,
        });

        let resp_text = self.post_exchange(&body).await?;

        let sent_cancels_owned: Vec<CancelIntent> =
            wires_with_idx.iter().map(|(i, _)| cancels[*i].clone()).collect();
        let parsed = parse_cancel_response(&resp_text, &sent_cancels_owned)?;

        for ((i, _), resp) in wires_with_idx.iter().zip(parsed) {
            responses[*i] = Some(resp);
        }

        Ok(responses.into_iter().flatten().collect())
    }
}

/// Parse HL `/exchange` response for an order action into per-order `OrderResponse`.
///
/// Recognized status shapes per element of `response.data.statuses`:
/// - `{"resting": {"oid": <u64>}}` -> status="resting"
/// - `{"filled": {"oid": <u64>, "totalSz": "...", "avgPx": "..."}}` -> status="filled"
/// - `{"error": "<msg>"}` -> status="error"
///
/// Render a `serde_json::Value` as a human-readable error string.
/// Strings are emitted verbatim; objects/arrays are rendered as compact JSON
/// so the diagnostic detail is preserved instead of being collapsed to "(no msg)".
fn json_to_err_string(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        s.to_string()
    } else {
        v.to_string()
    }
}

/// Top-level `{"status":"err", "response": "<msg>"}` returns `Err(HlError::Exchange)`.
fn parse_exchange_response(
    text: &str,
    orders: &[OrderIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse exchange json: {e}")))?;

    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v
            .get("response")
            .map(json_to_err_string)
            .unwrap_or_else(|| "(no msg)".into());
        return Err(HlError::Exchange {
            code: Some("top_level_err".into()),
            message: msg,
        });
    }

    let statuses = v
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| HlError::InvalidResponse("statuses missing".into()))?;

    if statuses.len() != orders.len() {
        return Err(HlError::InvalidResponse(format!(
            "statuses len {} != orders len {}",
            statuses.len(),
            orders.len()
        )));
    }

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(orders.iter()) {
        let cloid = intent.cloid;
        if let Some(resting) = status.get("resting") {
            let oid = resting
                .get("oid")
                .and_then(|o| o.as_u64())
                .ok_or_else(|| HlError::InvalidResponse("resting.oid missing".into()))?;
            out.push(OrderResponse {
                cloid,
                oid: Some(OrderId(oid)),
                status: "resting".into(),
                error: None,
            });
        } else if let Some(filled) = status.get("filled") {
            let oid = filled
                .get("oid")
                .and_then(|o| o.as_u64())
                .ok_or_else(|| HlError::InvalidResponse("filled.oid missing".into()))?;
            out.push(OrderResponse {
                cloid,
                oid: Some(OrderId(oid)),
                status: "filled".into(),
                error: None,
            });
        } else if let Some(err) = status.get("error") {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "error".into(),
                error: Some(json_to_err_string(err)),
            });
        } else {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "unknown".into(),
                error: Some(format!("unknown status shape: {status}")),
            });
        }
    }
    Ok(out)
}

/// Parse HL `/exchange` response for a cancelByCloid action.
///
/// HL returns cancel success as the bare string `"success"` (NOT an object,
/// unlike order responses). Per-cancel error is `{"error": "<msg>"}`.
fn parse_cancel_response(
    text: &str,
    cancels: &[CancelIntent],
) -> Result<Vec<OrderResponse>, HlError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| HlError::InvalidResponse(format!("parse cancel json: {e}")))?;

    if v.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = v
            .get("response")
            .map(json_to_err_string)
            .unwrap_or_else(|| "(no msg)".into());
        return Err(HlError::Exchange {
            code: Some("top_level_err".into()),
            message: msg,
        });
    }

    let statuses = v
        .pointer("/response/data/statuses")
        .and_then(|s| s.as_array())
        .ok_or_else(|| HlError::InvalidResponse("cancel statuses missing".into()))?;

    if statuses.len() != cancels.len() {
        return Err(HlError::InvalidResponse(format!(
            "cancel statuses len {} != cancels len {}",
            statuses.len(),
            cancels.len()
        )));
    }

    let mut out = Vec::with_capacity(statuses.len());
    for (status, intent) in statuses.iter().zip(cancels.iter()) {
        let cloid = intent.by_cloid.unwrap_or_default();
        if status.as_str() == Some("success") {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "cancelled".into(),
                error: None,
            });
        } else if let Some(err) = status.get("error") {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "error".into(),
                error: Some(json_to_err_string(err)),
            });
        } else {
            out.push(OrderResponse {
                cloid,
                oid: None,
                status: "unknown".into(),
                error: Some(format!("unknown cancel status shape: {status}")),
            });
        }
    }
    Ok(out)
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
            asset: 0,
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
            asset: 0,
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
