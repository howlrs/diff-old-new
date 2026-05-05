//! Pluggable pre-flight check for `OrderIntent` placements.
//!
//! `BatchSender` consults a gate before enqueueing an `OrderOrCancel::Place`.
//! `OrderOrCancel::Cancel` is never gated — emergency_stop must always work.
//! The concrete check (e.g. symbol allow-list + size cap) lives in
//! `executor-server::safety::SafetyGate` and impls this trait.

use executor_core::intent::OrderIntent;

pub trait IntentChecker: std::fmt::Debug + Send + Sync + 'static {
    /// Inspect the intent. Return `Err(reason)` to drop, `Ok(())` to allow.
    fn check_place(&self, intent: &OrderIntent) -> Result<(), String>;
}
