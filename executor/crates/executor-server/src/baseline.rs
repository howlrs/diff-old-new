//! BaselineGuard: PR-C3 baseline-diff guard.
//!
//! At startup, [`BaselineGuard::capture`] takes a snapshot of master EOA perp
//! positions across the configured dexes. The tick task in `main.rs` then
//! calls [`BaselineGuard::check_once`] every `poll_interval` and forwards any
//! reported [`BaselineViolation`]s to `routes::execute_emergency_stop`.
//!
//! Design notes (Gemini deep, 2026-05-05):
//! - The baseline is read-only after capture, so a plain `HashMap` (no
//!   `RwLock`) is enough. Cheap reads, no contention.
//! - Key shape is `(Option<String>, Symbol)` rather than a string-prefixed
//!   `Symbol` so callers can't accidentally compare `xyz:META` against `META`.
//! - [`BaselineGuard::check_once`] returns `Err(_)` only when the underlying
//!   `fetch_account_state` fails. The tick-task caller decides whether a
//!   transient failure should count toward the consecutive-error threshold.
//! - Disappeared positions (baseline had a non-zero szi, the current snapshot
//!   has none) are also surfaced as violations — silent close is treated the
//!   same as size drift.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Context as _;
use rust_decimal::Decimal;
use thiserror::Error;

use executor_core::symbol::Symbol;
use executor_core::types::Address;
use executor_hl::hl_client::HlClient;
use executor_hl::HlError;

/// `(dex, symbol)` key for the baseline map. `None` = default perp dex,
/// `Some("xyz")` = HIP-3 builder dex.
pub type BaselineKey = (Option<String>, Symbol);

#[derive(Debug)]
pub struct BaselineGuard {
    /// Read-only after [`BaselineGuard::capture`].
    pub baseline: HashMap<BaselineKey, Decimal>,
    pub master: Address,
    pub dexes: Vec<Option<String>>,
    pub poll_interval: Duration,
    pub szi_epsilon: Decimal,
    /// PR-D6: symbols the algorithm is explicitly allowed to trade. Their
    /// position is expected to drift, so they are NOT captured into the
    /// baseline AND NOT compared in `check_once`. This is the only sane
    /// behavior for the "passive_follow build" use case observed live on
    /// 2026-05-05 (PR-D1 POSTMORTEM §6.2): the user grants the executor a
    /// budget on ETH via `--mainnet-allow-symbols ETH`, so the guard must
    /// stop treating ETH drift as a violation. Symbols outside this set
    /// (e.g. HYPE / TON / xyz:META in the live test) are still strictly
    /// guarded because the operator hasn't authorised the bot to touch them.
    pub excluded_symbols: HashSet<Symbol>,
}

#[derive(Debug, Clone, Error)]
#[error(
    "baseline_violation: dex={dex:?} symbol={symbol} \
     baseline={baseline_szi} current={current_szi} diff={diff}"
)]
pub struct BaselineViolation {
    pub dex: Option<String>,
    pub symbol: Symbol,
    pub baseline_szi: Decimal,
    pub current_szi: Decimal,
    pub diff: Decimal,
}

impl BaselineGuard {
    pub async fn capture<C>(
        client: &C,
        master: Address,
        dexes: Vec<Option<String>>,
        poll_interval: Duration,
        szi_epsilon: Decimal,
        excluded_symbols: HashSet<Symbol>,
    ) -> anyhow::Result<Self>
    where
        C: HlClient + ?Sized,
    {
        let mut baseline: HashMap<BaselineKey, Decimal> = HashMap::new();
        for dex in &dexes {
            let snap = client
                .fetch_account_state(&master, dex.as_deref())
                .await
                .with_context(|| {
                    format!("BaselineGuard::capture: fetch_account_state failed for dex={dex:?}")
                })?;
            for (sym, pos) in &snap.positions {
                if excluded_symbols.contains(sym) {
                    // PR-D6: skip allow-listed trading symbols entirely. We
                    // never put them in the baseline so check_once won't
                    // produce a violation when their size moves.
                    tracing::info!(
                        symbol = %sym,
                        baseline_skip = "allowlist",
                        "BaselineGuard::capture: skipping allow-listed symbol"
                    );
                    continue;
                }
                baseline.insert((dex.clone(), sym.clone()), pos.size);
            }
        }
        Ok(Self {
            baseline,
            master,
            dexes,
            poll_interval,
            szi_epsilon,
            excluded_symbols,
        })
    }

    pub async fn check_once<C>(&self, client: &C) -> Result<Vec<BaselineViolation>, HlError>
    where
        C: HlClient + ?Sized,
    {
        let mut violations: Vec<BaselineViolation> = Vec::new();
        let mut seen: HashSet<BaselineKey> = HashSet::new();
        for dex in &self.dexes {
            let snap = client
                .fetch_account_state(&self.master, dex.as_deref())
                .await?;
            for (sym, pos) in &snap.positions {
                if self.excluded_symbols.contains(sym) {
                    // PR-D6: ignore drift on allow-listed symbols.
                    continue;
                }
                let key: BaselineKey = (dex.clone(), sym.clone());
                seen.insert(key.clone());
                let baseline_szi = self.baseline.get(&key).copied().unwrap_or(Decimal::ZERO);
                let diff = (pos.size - baseline_szi).abs();
                if diff > self.szi_epsilon {
                    violations.push(BaselineViolation {
                        dex: dex.clone(),
                        symbol: sym.clone(),
                        baseline_szi,
                        current_szi: pos.size,
                        diff,
                    });
                }
            }
        }
        // Detect positions that vanished between capture and now.
        for (key, baseline_szi) in &self.baseline {
            if !seen.contains(key) && *baseline_szi != Decimal::ZERO {
                violations.push(BaselineViolation {
                    dex: key.0.clone(),
                    symbol: key.1.clone(),
                    baseline_szi: *baseline_szi,
                    current_szi: Decimal::ZERO,
                    diff: baseline_szi.abs(),
                });
            }
        }
        Ok(violations)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use executor_core::state::Position;
    use executor_hl::hl_client::{AccountStateSnapshot, MockHlClient};
    use rust_decimal_macros::dec;

    fn snap_with(address: &Address, items: &[(&str, Decimal)]) -> AccountStateSnapshot {
        let mut snap = AccountStateSnapshot::empty(address.clone());
        for (sym, sz) in items {
            snap.positions.insert(
                Symbol::new(*sym),
                Position {
                    size: *sz,
                    ..Default::default()
                },
            );
        }
        snap
    }

    #[tokio::test]
    async fn capture_succeeds_with_seeded_positions() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        // Seed default-dex positions. Mock currently ignores dex param so
        // both dexes return the same snapshot — for unit tests we only verify
        // the default dex.
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10)), ("ETH", dec!(0))]));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            HashSet::new(),
        )
        .await
        .unwrap();
        assert_eq!(g.baseline.len(), 2);
        assert_eq!(
            g.baseline.get(&(None, Symbol::new("HYPE"))).copied(),
            Some(dec!(10))
        );
        assert_eq!(
            g.baseline.get(&(None, Symbol::new("ETH"))).copied(),
            Some(dec!(0))
        );
    }

    #[tokio::test]
    async fn check_once_returns_empty_when_unchanged() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10))]));
        let g = BaselineGuard::capture(
            &mock,
            master,
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            HashSet::new(),
        )
        .await
        .unwrap();
        let v = g.check_once(&mock).await.unwrap();
        assert!(
            v.is_empty(),
            "unchanged baseline must produce no violations"
        );
    }

    #[tokio::test]
    async fn check_once_detects_size_increase() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10))]));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            HashSet::new(),
        )
        .await
        .unwrap();
        // Mutate the mock snapshot so the next fetch returns size=11.
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(11))]));
        let v = g.check_once(&mock).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].symbol, Symbol::new("HYPE"));
        assert_eq!(v[0].baseline_szi, dec!(10));
        assert_eq!(v[0].current_szi, dec!(11));
        assert_eq!(v[0].diff, dec!(1));
    }

    #[tokio::test]
    async fn check_once_detects_position_disappearance() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10))]));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            HashSet::new(),
        )
        .await
        .unwrap();
        // Position disappeared.
        mock.seed_account(snap_with(&master, &[]));
        let v = g.check_once(&mock).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].symbol, Symbol::new("HYPE"));
        assert_eq!(v[0].current_szi, Decimal::ZERO);
        assert_eq!(v[0].diff, dec!(10));
    }

    #[tokio::test]
    async fn check_once_with_szi_epsilon_tolerates_small_drift() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10))]));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            dec!(0.01),
            HashSet::new(),
        )
        .await
        .unwrap();
        // Drift = 0.005, under epsilon 0.01.
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10.005))]));
        let v = g.check_once(&mock).await.unwrap();
        assert!(v.is_empty(), "drift below epsilon must be ignored");
    }

    #[tokio::test]
    async fn check_once_propagates_fetch_error() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(&master, &[("HYPE", dec!(10))]));
        let g = BaselineGuard::capture(
            &mock,
            master,
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            HashSet::new(),
        )
        .await
        .unwrap();
        mock.set_fail_account_state(true);
        let err = g
            .check_once(&mock)
            .await
            .expect_err("forced fetch failure must propagate");
        let msg = format!("{err}");
        assert!(msg.contains("forced failure"), "unexpected error: {msg}");
    }

    /// PR-D6: capture must skip excluded symbols entirely so subsequent
    /// drift on those symbols never produces a violation. Live observation
    /// 2026-05-05: with `--mainnet-allow-symbols ETH`, a +0.005 ETH place
    /// (round 1 of build) tripped BaselineGuard because ETH was captured
    /// as 0.1 → drifted to 0.105 → diff > 0.
    #[tokio::test]
    async fn capture_skips_excluded_symbols() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(
            &master,
            &[("ETH", dec!(0.1)), ("HYPE", dec!(10))],
        ));
        let mut excluded = HashSet::new();
        excluded.insert(Symbol::new("ETH"));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            excluded,
        )
        .await
        .unwrap();
        // ETH excluded → not in baseline. HYPE captured.
        assert!(!g.baseline.contains_key(&(None, Symbol::new("ETH"))));
        assert_eq!(
            g.baseline.get(&(None, Symbol::new("HYPE"))).copied(),
            Some(dec!(10))
        );
    }

    /// PR-D6: drift on an excluded symbol produces no violation, even when
    /// drift on a non-excluded symbol still does.
    #[tokio::test]
    async fn check_once_ignores_excluded_symbol_drift() {
        let mock = MockHlClient::new();
        let master = Address::new("0xfeedface");
        mock.seed_account(snap_with(
            &master,
            &[("ETH", dec!(0.1)), ("HYPE", dec!(10))],
        ));
        let mut excluded = HashSet::new();
        excluded.insert(Symbol::new("ETH"));
        let g = BaselineGuard::capture(
            &mock,
            master.clone(),
            vec![None],
            Duration::from_secs(60),
            Decimal::ZERO,
            excluded,
        )
        .await
        .unwrap();
        // ETH grew (algorithm built position) AND HYPE drifted. Only HYPE
        // should be flagged.
        mock.seed_account(snap_with(
            &master,
            &[("ETH", dec!(0.2)), ("HYPE", dec!(11))],
        ));
        let v = g.check_once(&mock).await.unwrap();
        assert_eq!(v.len(), 1, "only HYPE should violate; got {v:?}");
        assert_eq!(v[0].symbol, Symbol::new("HYPE"));
    }

    #[tokio::test]
    async fn baseline_violation_implements_display() {
        let v = BaselineViolation {
            dex: Some("xyz".into()),
            symbol: Symbol::new("META"),
            baseline_szi: dec!(5),
            current_szi: dec!(6),
            diff: dec!(1),
        };
        let s = format!("{v}");
        assert!(s.contains("META") && s.contains("baseline=5") && s.contains("current=6"));
    }
}
