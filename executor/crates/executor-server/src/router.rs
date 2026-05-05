//! `OrderRouter` — name → Algorithm dispatcher.
//!
//! Selects the right `Algorithm` impl from a string name, so the REST/CLI
//! surface can refer to algorithms by their wire name ("market", "twap", ...).

use std::collections::HashSet;

use executor_algo::{
    Algorithm, MarketAlgorithm, MarketMakeAlgorithm, PassiveFollowAlgorithm, TwapAlgorithm,
};
use executor_core::errors::ExecutorError;

/// Build a fresh boxed Algorithm from its wire name. Used by the server when
/// kicking off a new execution.
pub struct OrderRouter;

impl OrderRouter {
    pub fn build(name: &str) -> Result<Box<dyn Algorithm>, ExecutorError> {
        let normalized = name.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "market" => Ok(Box::new(MarketAlgorithm::new())),
            "passive" | "passive_follow" => Ok(Box::new(PassiveFollowAlgorithm::new())),
            "twap" => Ok(Box::new(TwapAlgorithm::new())),
            "market_make" | "mm" => Ok(Box::new(MarketMakeAlgorithm::new())),
            other => Err(ExecutorError::InvalidRequest(format!(
                "unknown algorithm: '{other}'. supported: market / passive / twap / market_make"
            ))),
        }
    }

    /// Reportable list of supported algorithms (for /v1/health / docs).
    pub fn supported() -> Vec<&'static str> {
        vec!["market", "passive", "twap", "market_make"]
    }
}

/// Validate that the requested algorithm name is supported. Returns an error
/// when not (rather than panicking when the server tries to spawn it).
pub fn validate_algorithm_name(name: &str) -> Result<(), ExecutorError> {
    let supported: HashSet<&'static str> = OrderRouter::supported().into_iter().collect();
    let normalized = name.trim().to_ascii_lowercase();
    let canonical = match normalized.as_str() {
        "passive_follow" => "passive",
        "mm" => "market_make",
        other => other,
    };
    if supported.contains(canonical) {
        Ok(())
    } else {
        Err(ExecutorError::InvalidRequest(format!(
            "unknown algorithm: '{name}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn build_each_supported() {
        for n in [
            "market",
            "passive",
            "passive_follow",
            "twap",
            "market_make",
            "mm",
        ] {
            let algo = OrderRouter::build(n).unwrap();
            assert!(!algo.name().is_empty());
        }
    }

    #[test]
    fn build_unknown_errors() {
        assert!(OrderRouter::build("vwap").is_err());
        assert!(OrderRouter::build("").is_err());
    }

    #[test]
    fn build_is_case_insensitive() {
        assert!(OrderRouter::build("MARKET").is_ok());
        assert!(OrderRouter::build("Twap").is_ok());
        assert!(OrderRouter::build("Market_Make").is_ok());
    }

    #[test]
    fn validate_accepts_synonyms() {
        assert!(validate_algorithm_name("passive_follow").is_ok());
        assert!(validate_algorithm_name("mm").is_ok());
    }

    #[test]
    fn validate_rejects_typos() {
        assert!(validate_algorithm_name("twapp").is_err());
    }
}
