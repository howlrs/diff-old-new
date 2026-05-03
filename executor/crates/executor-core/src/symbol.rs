//! Symbol identifier (e.g. "BTC", "ETH", "xyz:SP500", "xyz:XYZ100").

use serde::{Deserialize, Serialize};

/// Hyperliquid coin symbol.
///
/// HIP-3 deployer markets are prefixed with the dex name, e.g. `xyz:SP500`.
/// Standard validator-operated markets are bare, e.g. `BTC`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Symbol(pub String);

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// HIP-3 builder dex name (e.g. "xyz") if prefixed; else None for core perps.
    pub fn dex(&self) -> Option<&str> {
        self.0.split(':').next().filter(|_| self.0.contains(':'))
    }

    /// True iff this symbol belongs to a builder-deployed dex (HIP-3).
    pub fn is_hip3(&self) -> bool {
        self.0.contains(':')
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn core_perp_has_no_dex() {
        let s = Symbol::new("BTC");
        assert_eq!(s.dex(), None);
        assert!(!s.is_hip3());
    }

    #[test]
    fn xyz_perp_has_dex() {
        let s = Symbol::new("xyz:SP500");
        assert_eq!(s.dex(), Some("xyz"));
        assert!(s.is_hip3());
    }

    #[test]
    fn display_roundtrip() {
        let s = Symbol::from("ETH");
        assert_eq!(format!("{s}"), "ETH");
        assert_eq!(s.as_str(), "ETH");
    }
}
