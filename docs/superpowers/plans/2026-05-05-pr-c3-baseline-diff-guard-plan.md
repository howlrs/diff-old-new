# PR-C3: baseline-diff guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans.

**Goal:** Add a `BaselineGuard` to executor-server that captures master-EOA perp positions across configured dexes at startup and re-fetches them on a fixed interval (default 60s). Any size deviation fires `execute_emergency_stop`. The kill-switch is now idempotent (an `AtomicBool` on `ServerState` ensures only the first caller does the cancel/abort work; subsequent calls are no-ops). New `start_exec` requests after shutdown return 503. The guard is **real-mode only** — mock mode keeps the no-guard path that PR-C2 already validated.

**Architecture:** A new `BaselineGuard` struct lives in `executor-server::baseline`. It holds the immutable startup `HashMap<(Option<String>, Symbol), Decimal>` baseline and the polling configuration. `capture()` does a one-shot fetch at startup; `check_once()` does the periodic re-fetch and returns a list of `BaselineViolation`s. The tick task is spawned in `main.rs` and uses `tokio::select!` over `ticker.tick()` and a shutdown watch channel; it tracks consecutive fetch errors locally and fires `execute_emergency_stop` after a configurable threshold (`--baseline-max-consec-errors`, default 5). The HTTP `emergency_stop` handler and the guard share a single `execute_emergency_stop(state, operator)` function whose first-line is an `AtomicBool::compare_exchange` for idempotency. `start_exec` reads the same `AtomicBool` and returns 503 (`ServerError::ServiceUnavailable`) when the executor is in stopped state.

**Tech Stack:** Rust 2021 (workspace MSRV 1.91). New deps: none — `std::sync::atomic::AtomicBool` is already in std, `tokio::sync::watch` and `tokio::time::interval` are already in tokio.

---

## File Structure

| Path | Action | Notes |
|---|---|---|
| `executor/crates/executor-server/src/baseline.rs` | Create | `BaselineGuard`, `BaselineViolation`, `BaselineKey` type alias. ~150 LOC + 6 unit tests. |
| `executor/crates/executor-server/src/lib.rs` | Modify | `pub mod baseline; pub use baseline::{BaselineGuard, BaselineViolation};`. |
| `executor/crates/executor-server/src/error.rs` | Modify | Add `ServiceUnavailable(String)` variant + 503 mapping. |
| `executor/crates/executor-server/src/state.rs` | Modify | Add `pub shutdown_initiated: AtomicBool`. Default-initialise in `ServerState::new` (no signature change — internally `AtomicBool::new(false)`). |
| `executor/crates/executor-server/src/routes.rs` | Modify | Extract `execute_emergency_stop` (idempotency-gated). Refactor HTTP handler. Add 503 check at top of `start_exec`. |
| `executor/crates/executor-server/src/main.rs` | Modify | Add 6 CLI flags. After `MetaCache::build`, capture baseline + spawn tick task with `tokio::select!`. |
| `executor/crates/executor-server/tests/integration_rest.rs` | Modify | Add 2 tests for emergency_stop idempotency + 503 after stop. |
| `docs/HANDOFF-2026-05-05.md` | Modify | Append §11 PR-C3 完了 subsection. |

---

## Step 1: ServerError::ServiceUnavailable

- [ ] In `executor/crates/executor-server/src/error.rs`:
  - Add variant: `#[error("service unavailable: {0}")] ServiceUnavailable(String),`
  - Extend `IntoResponse::into_response` match: `ServerError::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", self.to_string()),`
- [ ] `cargo build -p executor-server`.

## Step 2: ServerState shutdown flag

- [ ] In `state.rs`:
  - Add `use std::sync::atomic::AtomicBool;` import.
  - Add field `pub shutdown_initiated: AtomicBool` to `ServerState`.
  - In `ServerState::new`, initialise `shutdown_initiated: AtomicBool::new(false)` (no parameter change — keep the existing 6-arg signature).
  - Update Debug impl (omit the AtomicBool to avoid noise; finish_non_exhaustive already covers).
- [ ] `cargo build -p executor-server`.

## Step 3: Extract execute_emergency_stop with idempotency

- [ ] In `routes.rs`:
  - New function:
    ```rust
    pub async fn execute_emergency_stop(
        s: &Arc<ServerState>,
        operator: &str,
    ) -> EmergencyStopResponse {
        use std::sync::atomic::Ordering;
        if s.shutdown_initiated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::info!(operator, "emergency_stop: already initiated, skipping");
            return EmergencyStopResponse {
                aborted_executions: 0,
                cancelled_orders: 0,
            };
        }
        // existing body of emergency_stop, refactored
    }
    ```
  - Modify `emergency_stop` HTTP handler to delegate:
    ```rust
    pub async fn emergency_stop(
        State(s): State<Arc<ServerState>>,
        headers: axum::http::HeaderMap,
    ) -> Result<Json<EmergencyStopResponse>, ServerError> {
        let operator = headers.get("x-operator-id").and_then(|v| v.to_str().ok()).unwrap_or("unknown").to_string();
        Ok(Json(execute_emergency_stop(&s, &operator).await))
    }
    ```
- [ ] In `start_exec`, after `validate_algorithm_name`, add:
  ```rust
  if s.shutdown_initiated.load(std::sync::atomic::Ordering::Acquire) {
      return Err(ServerError::ServiceUnavailable(
          "executor is in emergency_stop state; restart required".into(),
      ));
  }
  ```
- [ ] `cargo build -p executor-server`.

## Step 4: integration tests for idempotency + 503

- [ ] In `tests/integration_rest.rs`:
  - Add `emergency_stop_idempotent`:
    - First POST → expect at least 1 cancelled order (use the existing seed pattern with an open order).
    - Second POST → expect both fields = 0.
  - Add `start_exec_after_emergency_stop_503`:
    - Trigger emergency_stop → expect 200.
    - POST `/v1/exec` with valid payload → expect 503.
- [ ] `cargo test -p executor-server emergency_stop`.

## Step 5: BaselineGuard skeleton

- [ ] Create `executor/crates/executor-server/src/baseline.rs`:
  ```rust
  //! BaselineGuard: PR-C3 baseline-diff guard.
  //!
  //! At startup, captures master EOA perp positions across configured dexes.
  //! Periodic check_once() compares current to baseline; deviations are
  //! reported as `BaselineViolation`s.

  use std::collections::{HashMap, HashSet};
  use std::time::Duration;

  use rust_decimal::Decimal;
  use thiserror::Error;

  use executor_core::symbol::Symbol;
  use executor_core::types::Address;
  use executor_hl::hl_client::HlClient;
  use executor_hl::HlError;

  pub type BaselineKey = (Option<String>, Symbol);

  #[derive(Debug)]
  pub struct BaselineGuard {
      pub baseline: HashMap<BaselineKey, Decimal>,
      pub master: Address,
      pub dexes: Vec<Option<String>>,
      pub poll_interval: Duration,
      pub szi_epsilon: Decimal,
  }

  #[derive(Debug, Clone, Error)]
  #[error("baseline_violation: dex={dex:?} symbol={symbol} baseline={baseline_szi} current={current_szi} diff={diff}")]
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
      ) -> anyhow::Result<Self>
      where
          C: HlClient + ?Sized,
      {
          let mut baseline = HashMap::new();
          for dex in &dexes {
              let snap = client.fetch_account_state(&master, dex.as_deref()).await
                  .with_context(|| format!("BaselineGuard::capture: fetch_account_state failed for dex={:?}", dex))?;
              for (sym, pos) in &snap.positions {
                  baseline.insert((dex.clone(), sym.clone()), pos.size);
              }
          }
          Ok(Self { baseline, master, dexes, poll_interval, szi_epsilon })
      }

      pub async fn check_once<C>(&self, client: &C) -> Result<Vec<BaselineViolation>, HlError>
      where C: HlClient + ?Sized,
      {
          let mut violations = Vec::new();
          let mut seen: HashSet<BaselineKey> = HashSet::new();
          for dex in &self.dexes {
              let snap = client.fetch_account_state(&self.master, dex.as_deref()).await?;
              for (sym, pos) in &snap.positions {
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

  // ---- tests ----
  #[cfg(test)]
  mod tests {
      // 6 tests outlined in spec §5.1
  }
  ```
- [ ] In `lib.rs`: `pub mod baseline; pub use baseline::{BaselineGuard, BaselineViolation, BaselineKey};`
- [ ] `cargo build -p executor-server`.

## Step 6: BaselineGuard unit tests

- [ ] `MockHlClient::seed_account` is already exposed (`pub fn seed_account(&self, snap: AccountStateSnapshot)`). Use it.
- [ ] Tests in `baseline.rs::tests`:
  1. `capture_succeeds_with_seeded_positions`: seed default-dex with `HYPE` szi=10 + xyz dex with `META` szi=5 → capture builds the 2-entry map.
  2. `check_once_returns_empty_when_unchanged`: same seed → `check_once` = `Ok([])`.
  3. `check_once_detects_size_increase`: capture, then mutate `mock.account.lock().await.positions.get_mut(&Symbol::new("HYPE")).unwrap().size = 11` → 1 violation with `diff=1`.
  4. `check_once_detects_position_disappearance`: capture, then clear positions → 1 violation per non-zero baseline entry with `current_szi=0`.
  5. `check_once_with_szi_epsilon_tolerates_small_drift`: epsilon=`0.01`, diff=`0.005` → `Ok([])`.
  6. `check_once_propagates_fetch_error`: This requires `MockHlClient` to support a "next fetch fails" knob; if not present, mark as `ignored` with TODO and rely on integration test on real client (see plan note below).

  Plan note for test 6: `MockHlClient` currently always returns the seeded state. To test fetch-error propagation, we can either:
   - extend `MockHlClient` with `pub fail_next_fetch: AtomicBool` and `set_fail(true)` (small change), OR
   - skip and use mockito-based tests in `RealHlClient` like `place_cancel_mock.rs` (heavier).

  For PR-C3 we extend `MockHlClient` with `pub fail_account_state: AtomicBool` (add it as a public field, default `false`, checked at the top of `fetch_account_state`). This is a 5-line change confined to `MockHlClient` and unblocks the test.

- [ ] `cargo test -p executor-server baseline`.

## Step 7: MockHlClient::fail_account_state

- [ ] In `executor-hl/src/hl_client.rs::MockHlClient`:
  - Add field `pub fail_account_state: std::sync::atomic::AtomicBool`. Initialise `AtomicBool::new(false)` in `new()`.
  - In `fetch_account_state`, at the top:
    ```rust
    if self.fail_account_state.load(std::sync::atomic::Ordering::Acquire) {
        return Err(HlError::Network("mock: fetch_account_state forced failure".into()));
    }
    ```
- [ ] `cargo build -p executor-hl`.
- [ ] Unblocks test 6.

## Step 8: main.rs CLI flags + tick task

- [ ] In `main.rs::Args`:
  ```rust
  /// Enable baseline-diff guard (real mode only).
  #[arg(long, env = "EXECUTOR_BASELINE_GUARD", default_value_t = true)]
  baseline_guard: bool,

  /// Master EOA address to monitor.
  #[arg(long, env = "HL_MASTER_ADDRESS")]
  master_address: Option<String>,

  #[arg(long, env = "EXECUTOR_BASELINE_POLL_SECS", default_value_t = 60)]
  baseline_poll_secs: u64,

  #[arg(long, env = "EXECUTOR_BASELINE_DEXES", default_value = "default,xyz")]
  baseline_dexes: String,

  #[arg(long, env = "EXECUTOR_BASELINE_SZI_EPSILON", default_value = "0")]
  baseline_szi_epsilon: String,

  #[arg(long, env = "EXECUTOR_BASELINE_MAX_CONSEC_ERRORS", default_value_t = 5)]
  baseline_max_consec_errors: u32,
  ```
- [ ] After `MetaCache::build` and `RealHlClient::with_meta`, add baseline capture (real mode only):
  ```rust
  let baseline = if matches!(args.mode, Mode::Real) && args.baseline_guard {
      let master = args.master_address.as_deref()
          .context("--master-address (or HL_MASTER_ADDRESS env) required for real mode + baseline_guard")?;
      let dexes = parse_dexes_csv(&args.baseline_dexes);
      let eps: Decimal = args.baseline_szi_epsilon.parse().unwrap_or(Decimal::ZERO);
      let g = BaselineGuard::capture(
          real_client.as_ref(),
          Address::new(master),
          dexes,
          Duration::from_secs(args.baseline_poll_secs),
          eps,
      ).await.context("BaselineGuard::capture failed")?;
      tracing::info!(
          master = master,
          dexes = ?g.dexes,
          baseline_size = g.baseline.len(),
          poll_secs = g.poll_interval.as_secs(),
          szi_epsilon = ?g.szi_epsilon,
          "BaselineGuard captured",
      );
      Some(Arc::new(g))
  } else { None };
  ```
- [ ] Spawn the tick task after `state` is created:
  ```rust
  let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
  if let Some(g) = baseline.clone() {
      let st = state.clone();
      let client = real_client.clone();
      let max_consec = args.baseline_max_consec_errors;
      let mut shutdown_rx = shutdown_rx.clone();
      tokio::spawn(async move {
          let mut ticker = tokio::time::interval(g.poll_interval);
          ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
          let mut consec_errors: u32 = 0;
          loop {
              tokio::select! {
                  _ = ticker.tick() => {
                      match g.check_once(client.as_ref()).await {
                          Ok(violations) if violations.is_empty() => {
                              consec_errors = 0;
                              tracing::trace!("baseline_guard: tick clean");
                          }
                          Ok(violations) => {
                              tracing::error!(?violations, "BASELINE VIOLATION DETECTED");
                              executor_server::routes::execute_emergency_stop(&st, "baseline_guard").await;
                              tracing::error!("baseline_guard: emergency_stop fired, exiting tick loop");
                              break;
                          }
                          Err(e) => {
                              consec_errors += 1;
                              tracing::warn!(error = %e, consec_errors, "baseline_guard: fetch failed");
                              if consec_errors >= max_consec {
                                  tracing::error!(consec_errors, "baseline_guard: too many consecutive failures, firing emergency_stop");
                                  executor_server::routes::execute_emergency_stop(&st, "baseline_guard_consec_errors").await;
                                  break;
                              }
                          }
                      }
                  }
                  _ = shutdown_rx.changed() => {
                      if *shutdown_rx.borrow() {
                          tracing::info!("baseline_guard: shutdown signal received, exiting");
                          break;
                      }
                  }
              }
          }
      });
  }
  // shutdown_tx is dropped here; ctrl+c terminates the process.
  // Future PR can wire signal handling and shutdown_tx.send(true).
  let _ = shutdown_tx;  // suppress warning until signal handling is wired
  ```
- [ ] Helper `fn parse_dexes_csv(s: &str) -> Vec<Option<String>>` parses `"default,xyz"` → `[None, Some("xyz")]` (case-insensitive, `default` and empty → `None`).
- [ ] Need `Address::new` import: `use executor_core::types::Address;`. (Verify the actual path — adjust if it lives elsewhere.)
- [ ] `cargo build -p executor-server`.

## Step 9: Update routes::execute_emergency_stop visibility

- [ ] `execute_emergency_stop` must be accessible from `main.rs` via `executor_server::routes::execute_emergency_stop`. Mark it `pub` and ensure `routes` is `pub mod routes;` in lib.rs (already true).
- [ ] If routes is a private module, expose just the function via `pub use routes::execute_emergency_stop;` in lib.rs.

## Step 10: Workspace checks

- [ ] `cargo fmt --all`.
- [ ] `cargo build --workspace`.
- [ ] `cargo test --workspace` — expect 164 → ~172.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] `bash scripts/check_ci_local.sh`.

## Step 11: Manual offline check

- [ ] `cargo run -p executor-server -- --help` → 6 new flags shown.
- [ ] (User, separate terminal):
  - `cargo run -p executor-server -- --mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` (no --master-address, no HL_MASTER_ADDRESS env) → fatal: required.
  - `source scripts/load-env.sh && cargo run -p executor-server -- --mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` → "BaselineGuard captured master=... baseline_size=N" log.

## Step 12: Doc + commit + push + PR

- [ ] Append `## 11. PR-C3 完了` subsection to `docs/HANDOFF-2026-05-05.md`.
- [ ] Branch: `git checkout -b feat/pr-c3-baseline-diff-guard`.
- [ ] Commits split: spec, plan, impl (BaselineGuard + state + error + routes refactor + main + tests), HANDOFF.
- [ ] `git push -u origin feat/pr-c3-baseline-diff-guard`.
- [ ] `gh pr create --base develop --title "feat(executor): PR-C3 — baseline-diff guard + idempotent emergency_stop" --body ...`.
- [ ] Wait for CI green; self-merge per pre-v1.0 branch strategy.

## Acceptance gates

- [ ] All tests pass.
- [ ] No clippy warnings.
- [ ] `BaselineGuard captured` log on real-mode startup.
- [ ] `emergency_stop` is idempotent (2nd call is a no-op).
- [ ] `start_exec` returns 503 after stop.
- [ ] PR description references the Gemini deep review (5 SHOULD-FIX taken in).
