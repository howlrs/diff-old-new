//! Common primitive types: Address, Side, Tif, OrderId, Fill.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::cloid::Cloid;
use crate::symbol::Symbol;

/// Ethereum-style address. Wire format: lowercase 0x-prefixed 40 hex chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Address(pub String);

impl Address {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn as_signed_unit(self) -> Decimal {
        match self {
            Side::Long => Decimal::ONE,
            Side::Short => Decimal::NEGATIVE_ONE,
        }
    }
}

/// Time-in-force flag (matches Hyperliquid wire values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tif {
    /// Add liquidity only (post-only). Cancelled if would match immediately.
    Alo,
    /// Immediate-or-cancel. Unfilled remainder is cancelled.
    Ioc,
    /// Good til cancelled.
    Gtc,
}

/// Hyperliquid-side order id (returned by exchange after place success).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderId(pub u64);

impl std::fmt::Display for OrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A trade fill (one execution event).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub symbol: Symbol,
    pub cloid: Option<Cloid>,
    pub oid: OrderId,
    pub side: Side,
    pub px: Decimal,
    pub sz: Decimal,
    pub fee: Decimal,
    pub ts: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn side_signed_unit() {
        assert_eq!(Side::Long.as_signed_unit(), Decimal::ONE);
        assert_eq!(Side::Short.as_signed_unit(), Decimal::NEGATIVE_ONE);
    }

    #[test]
    fn tif_serializes_pascalcase() {
        // HL wire format expects "Alo", "Ioc", "Gtc"
        assert_eq!(serde_json::to_string(&Tif::Alo).unwrap(), "\"Alo\"");
        assert_eq!(serde_json::to_string(&Tif::Ioc).unwrap(), "\"Ioc\"");
        assert_eq!(serde_json::to_string(&Tif::Gtc).unwrap(), "\"Gtc\"");
    }
}
