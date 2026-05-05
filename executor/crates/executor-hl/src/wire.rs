//! HL `/info` endpoint JSON wire types.
//!
//! These structs mirror the on-the-wire shape exactly (camelCase JSON,
//! string-encoded numerics). Domain mapping into `executor_core::state`
//! lives in `hl_client.rs`.
//!
//! All numeric fields use `#[serde(with = "rust_decimal::serde::str")]`
//! because HL serializes prices/sizes as JSON strings to preserve precision.

use chrono::{TimeZone, Utc};
use executor_core::state::{BookLevel, OrderBook};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// HL `clearinghouseState` response (perp).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireClearinghouseState {
    pub margin_summary: WireMarginSummary,
    pub cross_margin_summary: WireMarginSummary,
    #[serde(with = "rust_decimal::serde::str")]
    pub cross_maintenance_margin_used: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub withdrawable: Decimal,
    pub asset_positions: Vec<WireAssetPosition>,
    pub time: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMarginSummary {
    #[serde(with = "rust_decimal::serde::str")]
    pub account_value: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_ntl_pos: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_raw_usd: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_margin_used: Decimal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAssetPosition {
    #[serde(rename = "type")]
    pub position_type: String, // "oneWay" today
    pub position: WirePosition,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WirePosition {
    pub coin: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub szi: Decimal,
    pub leverage: WireLeverage,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub position_value: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub return_on_equity: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub liquidation_px: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::str")]
    pub margin_used: Decimal,
    pub max_leverage: u32,
    pub cum_funding: WireCumFunding,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireLeverage {
    #[serde(rename = "type")]
    pub leverage_type: WireLeverageType,
    pub value: u32,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub raw_usd: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireLeverageType {
    Cross,
    Isolated,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireCumFunding {
    #[serde(with = "rust_decimal::serde::str")]
    pub all_time: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub since_open: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub since_change: Decimal,
}

/// HL `openOrders` array element.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireOpenOrder {
    pub coin: String,
    pub side: WireOrderSide,
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub oid: u64,
    /// HL wire is ms epoch (number).
    pub timestamp: i64,
}

/// HL `frontendOpenOrders` array element (superset of openOrders).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireFrontendOpenOrder {
    pub coin: String,
    pub side: WireOrderSide,
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub oid: u64,
    pub timestamp: i64,
    /// Order type label, e.g. "Limit", "Trigger". Free string in spec.
    pub order_type: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub orig_sz: Decimal,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub is_trigger: bool,
    #[serde(default)]
    pub is_position_tpsl: bool,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub trigger_px: Option<Decimal>,
    #[serde(default)]
    pub trigger_condition: Option<String>,
}

/// HL wire side: A = ask = sell, B = bid = buy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum WireOrderSide {
    A,
    B,
}

/// HL `l2Book` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireL2Book {
    pub coin: String,
    /// ms epoch of snapshot.
    pub time: i64,
    /// Two arrays: \[bids, asks\]. Bids descending, asks ascending.
    pub levels: Vec<Vec<WireBookLevel>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireBookLevel {
    #[serde(with = "rust_decimal::serde::str")]
    pub px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sz: Decimal,
    pub n: u32,
}

impl From<&WireBookLevel> for BookLevel {
    fn from(w: &WireBookLevel) -> Self {
        Self {
            px: w.px,
            sz: w.sz,
            n: w.n,
        }
    }
}

impl WireL2Book {
    /// Map into the domain `OrderBook` used by algorithms.
    ///
    /// HL guarantees `levels` is `[bids, asks]` with bids descending and
    /// asks ascending; if a malformed response arrives with fewer than two
    /// arrays, both sides are emitted empty so callers see "no quotes" rather
    /// than panicking.
    pub fn to_orderbook(&self) -> OrderBook {
        let bids = self
            .levels
            .first()
            .map(|v| v.iter().map(BookLevel::from).collect())
            .unwrap_or_default();
        let asks = self
            .levels
            .get(1)
            .map(|v| v.iter().map(BookLevel::from).collect())
            .unwrap_or_default();
        OrderBook {
            bids,
            asks,
            ts: Utc.timestamp_millis_opt(self.time).single(),
        }
    }
}

/// HL `meta` response (perp universe).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireMeta {
    pub universe: Vec<WireUniverseEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireUniverseEntry {
    pub name: String,
    pub sz_decimals: u32,
    pub max_leverage: u32,
    #[serde(default)]
    pub only_isolated: bool,
}

/// HL `userRole` response.
///
/// Wire shapes (from real responses):
/// - `{"role":"user"}`
/// - `{"role":"agent","data":{"user":"0x..."}}`
/// - `{"role":"vault"}` / `"subAccount"` / `"missing"` (data optional)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum WireUserRole {
    User,
    Agent { data: WireAgentData },
    Vault,
    SubAccount,
    Missing,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireAgentData {
    pub user: String,
}
