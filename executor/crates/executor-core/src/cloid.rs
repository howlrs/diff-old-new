//! Client-generated order id (cloid) — Gemini v2 review reflection.
//!
//! 16 bytes = 32 hex chars, uuid v7 (time-sortable + globally unique).
//! Lets the algorithm cancel an order *before* the exchange returns its `oid`.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Hyperliquid Client Order ID.
///
/// Wire format: 32 lowercase hex chars prefixed with "0x" (per HL exchange API).
/// Internally we keep a `Uuid` so we can sort by time (uuid v7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Cloid(Uuid);

impl Cloid {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    /// Hex-encoded with `0x` prefix (the HL wire format).
    pub fn to_hex_string(&self) -> String {
        format!("0x{}", self.0.simple())
    }

    pub fn uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for Cloid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Cloid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex_string())
    }
}

impl From<Cloid> for String {
    fn from(c: Cloid) -> Self {
        c.to_hex_string()
    }
}

impl TryFrom<String> for Cloid {
    type Error = uuid::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let trimmed = value.trim_start_matches("0x");
        Uuid::parse_str(trimmed).map(Self)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn cloid_is_unique() {
        let a = Cloid::new();
        let b = Cloid::new();
        assert_ne!(a, b);
    }

    #[test]
    fn cloid_hex_format_is_0x_prefixed_32_chars() {
        let c = Cloid::new();
        let s = c.to_hex_string();
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 34); // "0x" + 32 hex chars
    }

    #[test]
    fn cloid_serde_roundtrip() {
        let c = Cloid::new();
        let json = serde_json::to_string(&c).unwrap();
        let back: Cloid = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn cloid_v7_is_time_sorted() {
        // uuid v7 timestamps are monotonic within a process at ms resolution.
        let mut prev = Cloid::new();
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            let cur = Cloid::new();
            assert!(cur.uuid().as_u128() >= prev.uuid().as_u128());
            prev = cur;
        }
    }
}
