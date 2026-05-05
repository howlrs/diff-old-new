//! HL `/info` endpoint JSON wire types.
//!
//! These structs mirror the on-the-wire shape exactly (camelCase JSON,
//! string-encoded numerics). Domain mapping into `executor_core::state`
//! lives in `hl_client.rs`.
//!
//! All numeric fields use `#[serde(with = "rust_decimal::serde::str")]`
//! because HL serializes prices/sizes as JSON strings to preserve precision.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// HL `clearinghouseState` response (perp).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InfoClearinghouseState {
    #[serde(rename = "marginSummary")]
    pub margin_summary: WireMarginSummary,
    #[serde(rename = "crossMarginSummary")]
    pub cross_margin_summary: WireMarginSummary,
    #[serde(rename = "crossMaintenanceMarginUsed", with = "rust_decimal::serde::str")]
    pub cross_maintenance_margin_used: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub withdrawable: Decimal,
    #[serde(rename = "assetPositions")]
    pub asset_positions: Vec<WireAssetPosition>,
    pub time: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireMarginSummary {
    #[serde(rename = "accountValue", with = "rust_decimal::serde::str")]
    pub account_value: Decimal,
    #[serde(rename = "totalNtlPos", with = "rust_decimal::serde::str")]
    pub total_ntl_pos: Decimal,
    #[serde(rename = "totalRawUsd", with = "rust_decimal::serde::str")]
    pub total_raw_usd: Decimal,
    #[serde(rename = "totalMarginUsed", with = "rust_decimal::serde::str")]
    pub total_margin_used: Decimal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireAssetPosition {
    #[serde(rename = "type")]
    pub position_type: String, // "oneWay" today
    pub position: WirePosition,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WirePosition {
    pub coin: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub szi: Decimal,
    pub leverage: WireLeverage,
    #[serde(rename = "entryPx", with = "rust_decimal::serde::str")]
    pub entry_px: Decimal,
    #[serde(rename = "positionValue", with = "rust_decimal::serde::str")]
    pub position_value: Decimal,
    #[serde(rename = "unrealizedPnl", with = "rust_decimal::serde::str")]
    pub unrealized_pnl: Decimal,
    #[serde(rename = "returnOnEquity", with = "rust_decimal::serde::str")]
    pub return_on_equity: Decimal,
    #[serde(rename = "liquidationPx", default, with = "rust_decimal::serde::str_option")]
    pub liquidation_px: Option<Decimal>,
    #[serde(rename = "marginUsed", with = "rust_decimal::serde::str")]
    pub margin_used: Decimal,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(rename = "cumFunding")]
    pub cum_funding: WireCumFunding,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireLeverage {
    #[serde(rename = "type")]
    pub leverage_type: WireLeverageType,
    pub value: u32,
    #[serde(rename = "rawUsd", default, with = "rust_decimal::serde::str_option")]
    pub raw_usd: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireLeverageType {
    Cross,
    Isolated,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WireCumFunding {
    #[serde(rename = "allTime", with = "rust_decimal::serde::str")]
    pub all_time: Decimal,
    #[serde(rename = "sinceOpen", with = "rust_decimal::serde::str")]
    pub since_open: Decimal,
    #[serde(rename = "sinceChange", with = "rust_decimal::serde::str")]
    pub since_change: Decimal,
}
