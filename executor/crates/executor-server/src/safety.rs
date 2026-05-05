//! Mainnet safety gate: symbol allow-list + per-order USD notional cap.
//!
//! Wires into the BatchSender via the `IntentChecker` trait at startup.
//! Mock mode constructs a `disabled()` gate which short-circuits all checks.
//! Real mode requires `--mainnet-allow-symbols` to be set when targeting
//! mainnet; an empty value makes startup fatal (Gemini deep, 2026-05-05:
//! "warn ログは見落とされる. 金融系では明示 opt-in を要求する").
//!
//! Two layers fire on a place:
//! - **Layer 1** at `POST /v1/exec` via [`SafetyGate::check_request`] using
//!   `book.best_bid` as a rough reference price.
//! - **Layer 2** at [`executor_hl::BatchSender::enqueue`] via
//!   [`SafetyGate::check_intent`] using the order's actual `px`.
//!
//! `OrderOrCancel::Cancel` always bypasses the gate so emergency_stop works.

use std::collections::HashSet;

use rust_decimal::Decimal;
use thiserror::Error;

use executor_core::intent::OrderIntent;
use executor_core::symbol::Symbol;
use executor_hl::IntentChecker;

#[derive(Debug, Clone)]
pub struct SafetyGate {
    /// `None` = no symbol allow-list (everything passes).
    /// `Some(set)` = only symbols in the set are allowed.
    pub allow_symbols: Option<HashSet<Symbol>>,
    /// `None` = no notional cap. `Some(usd)` = max per-order notional (USD).
    pub max_notional_usd: Option<Decimal>,
}

#[derive(Debug, Clone, Error)]
pub enum SafetyViolation {
    #[error("symbol_not_allowed: symbol={symbol}, allowed={allowed:?}")]
    SymbolNotAllowed {
        symbol: Symbol,
        allowed: Vec<Symbol>,
    },
    #[error("notional_exceeded: symbol={symbol}, notional={notional}, max={max}")]
    NotionalExceeded {
        symbol: Symbol,
        notional: Decimal,
        max: Decimal,
    },
}

impl SafetyGate {
    /// Mock mode / explicit allow-all: every request passes.
    pub fn disabled() -> Self {
        Self {
            allow_symbols: None,
            max_notional_usd: None,
        }
    }

    /// Build from CLI args.
    ///
    /// `is_mainnet_real = (mode == real && base == mainnet)`. When that flag is
    /// `true` and `allow_csv` is empty, this returns an error so the binary
    /// refuses to start. The wildcard `"*"` is the explicit opt-in to allow-all.
    pub fn from_args(
        allow_csv: &str,
        max_usd: Option<u64>,
        is_mainnet_real: bool,
    ) -> anyhow::Result<Self> {
        let trimmed = allow_csv.trim();
        let allow_symbols = if trimmed.is_empty() {
            if is_mainnet_real {
                anyhow::bail!(
                    "--mainnet-allow-symbols is required for --mode real --base mainnet. \
                     Use '*' to explicitly allow-all (NOT recommended for production)."
                );
            }
            // Non-mainnet-real with empty CSV: keep as Some(empty) so the gate
            // can still be exercised on testnet (rejects everything).
            Some(HashSet::new())
        } else if trimmed == "*" {
            None
        } else {
            Some(
                trimmed
                    .split(',')
                    .map(|s| Symbol::new(s.trim().to_string()))
                    .filter(|s| !s.as_str().is_empty())
                    .collect(),
            )
        };
        Ok(Self {
            allow_symbols,
            max_notional_usd: max_usd.map(Decimal::from),
        })
    }

    /// Layer 1: REST-entry rough check using a reference price (best_bid).
    /// `ref_px = None` skips the notional check (no book yet) — the strict
    /// check happens at [`Self::check_intent`] anyway.
    pub fn check_request(
        &self,
        symbol: &Symbol,
        target_size: Decimal,
        ref_px: Option<Decimal>,
    ) -> Result<(), SafetyViolation> {
        if let Some(allowed) = &self.allow_symbols {
            if !allowed.contains(symbol) {
                return Err(SafetyViolation::SymbolNotAllowed {
                    symbol: symbol.clone(),
                    allowed: allowed.iter().cloned().collect(),
                });
            }
        }
        if let (Some(max), Some(px)) = (self.max_notional_usd, ref_px) {
            let notional = px * target_size;
            if notional > max {
                return Err(SafetyViolation::NotionalExceeded {
                    symbol: symbol.clone(),
                    notional,
                    max,
                });
            }
        }
        Ok(())
    }

    /// Layer 2: enqueue-time strict check using the order's actual px.
    pub fn check_intent(&self, o: &OrderIntent) -> Result<(), SafetyViolation> {
        if let Some(allowed) = &self.allow_symbols {
            if !allowed.contains(&o.symbol) {
                return Err(SafetyViolation::SymbolNotAllowed {
                    symbol: o.symbol.clone(),
                    allowed: allowed.iter().cloned().collect(),
                });
            }
        }
        if let Some(max) = self.max_notional_usd {
            let notional = o.px * o.sz;
            if notional > max {
                return Err(SafetyViolation::NotionalExceeded {
                    symbol: o.symbol.clone(),
                    notional,
                    max,
                });
            }
        }
        Ok(())
    }
}

impl IntentChecker for SafetyGate {
    fn check_place(&self, intent: &OrderIntent) -> Result<(), String> {
        self.check_intent(intent).map_err(|v| v.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use executor_core::cloid::Cloid;
    use executor_core::types::{Side, Tif};
    use rust_decimal_macros::dec;

    fn intent(symbol: &str, px: Decimal, sz: Decimal) -> OrderIntent {
        OrderIntent {
            cloid: Cloid::new(),
            symbol: Symbol::new(symbol),
            side: Side::Long,
            px,
            sz,
            tif: Tif::Gtc,
            reduce_only: false,
        }
    }

    #[test]
    fn disabled_passes_request() {
        let g = SafetyGate::disabled();
        g.check_request(&Symbol::new("XYZ"), dec!(100), Some(dec!(99999)))
            .expect("disabled gate should never reject");
    }

    #[test]
    fn disabled_passes_intent() {
        let g = SafetyGate::disabled();
        g.check_intent(&intent("XYZ", dec!(99999), dec!(1)))
            .expect("disabled gate should never reject");
    }

    #[test]
    fn allow_list_rejects_non_member() {
        let g = SafetyGate::from_args("ETH,BTC", None, false).unwrap();
        let err = g
            .check_request(&Symbol::new("XRP"), dec!(1), Some(dec!(1)))
            .expect_err("XRP must be rejected");
        assert!(matches!(err, SafetyViolation::SymbolNotAllowed { .. }));
    }

    #[test]
    fn allow_list_accepts_member() {
        let g = SafetyGate::from_args("ETH,BTC", None, false).unwrap();
        g.check_request(&Symbol::new("ETH"), dec!(0.001), Some(dec!(2400)))
            .expect("ETH must pass");
    }

    #[test]
    fn notional_cap_rejects_over() {
        let g = SafetyGate::from_args("ETH", Some(10), false).unwrap();
        // 0.005 * 2400 = 12 → over cap of 10
        let err = g
            .check_request(&Symbol::new("ETH"), dec!(0.005), Some(dec!(2400)))
            .expect_err("notional 12 > 10 must be rejected");
        match err {
            SafetyViolation::NotionalExceeded { notional, max, .. } => {
                assert_eq!(notional, dec!(12));
                assert_eq!(max, dec!(10));
            }
            other => panic!("unexpected violation: {other:?}"),
        }
    }

    #[test]
    fn notional_cap_accepts_under() {
        let g = SafetyGate::from_args("ETH", Some(10), false).unwrap();
        // 0.004 * 2400 = 9.6 → under cap
        g.check_request(&Symbol::new("ETH"), dec!(0.004), Some(dec!(2400)))
            .expect("notional 9.6 <= 10 must pass");
    }

    #[test]
    fn notional_check_skipped_without_ref_px() {
        let g = SafetyGate::from_args("ETH", Some(10), false).unwrap();
        // No ref_px → only allow-list check runs.
        g.check_request(&Symbol::new("ETH"), dec!(99999), None)
            .expect("missing ref_px must skip notional check");
    }

    #[test]
    fn check_intent_uses_actual_px() {
        let g = SafetyGate::from_args("ETH", Some(10), false).unwrap();
        // px=2400 sz=0.005 = 12 → reject
        let err = g
            .check_intent(&intent("ETH", dec!(2400), dec!(0.005)))
            .expect_err("notional 12 > 10 must be rejected");
        assert!(matches!(err, SafetyViolation::NotionalExceeded { .. }));
        // px=2400 sz=0.004 = 9.6 → ok
        g.check_intent(&intent("ETH", dec!(2400), dec!(0.004)))
            .expect("9.6 <= 10 must pass");
    }

    #[test]
    fn from_args_mainnet_real_empty_fatal() {
        let err = SafetyGate::from_args("", None, true)
            .expect_err("empty allow-list on mainnet+real must be fatal");
        let msg = format!("{err}");
        assert!(
            msg.contains("required") && msg.contains("mainnet"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn from_args_star_means_allow_all() {
        let g = SafetyGate::from_args("*", None, true).unwrap();
        assert!(
            g.allow_symbols.is_none(),
            "'*' must produce allow_symbols=None"
        );
        // Wildcard truly allows everything.
        g.check_request(&Symbol::new("WHATEVER"), dec!(1), Some(dec!(1)))
            .expect("'*' must accept any symbol");
    }

    #[test]
    fn from_args_csv_with_whitespace() {
        let g = SafetyGate::from_args("ETH , BTC ,LINK", None, false).unwrap();
        let allowed = g.allow_symbols.as_ref().unwrap();
        assert!(allowed.contains(&Symbol::new("ETH")));
        assert!(allowed.contains(&Symbol::new("BTC")));
        assert!(allowed.contains(&Symbol::new("LINK")));
        assert_eq!(allowed.len(), 3);
    }

    #[test]
    fn from_args_testnet_empty_keeps_empty_set() {
        // Testnet+real with empty CSV: not fatal, but rejects everything.
        let g = SafetyGate::from_args("", None, false).unwrap();
        assert_eq!(g.allow_symbols.as_ref().unwrap().len(), 0);
        let err = g
            .check_request(&Symbol::new("ETH"), dec!(1), None)
            .expect_err("empty allow-list (non-fatal mode) must still reject");
        assert!(matches!(err, SafetyViolation::SymbolNotAllowed { .. }));
    }

    #[test]
    fn from_args_max_notional_zero_treated_as_zero_cap() {
        // 0 is a valid (very strict) cap, not magic.
        let g = SafetyGate::from_args("ETH", Some(0), false).unwrap();
        assert_eq!(g.max_notional_usd, Some(Decimal::ZERO));
        let err = g
            .check_intent(&intent("ETH", dec!(1), dec!(0.001)))
            .expect_err("any notional > 0 must exceed cap of 0");
        assert!(matches!(err, SafetyViolation::NotionalExceeded { .. }));
    }
}
