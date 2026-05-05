# PR-C2: symbol allowlist + size cap (mainnet safety gate) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a 2-layer defense-in-depth safety gate to `executor-server` that rejects orders for non-allow-listed symbols and orders exceeding a configured per-order USD notional cap. Layer 1 fires at the REST entry (`POST /v1/exec`) using `book.best_bid` as a rough reference price. Layer 2 fires at `BatchSender::enqueue` time using the actual `OrderIntent.px`. Mock mode runs with the gate disabled (no test impact). Mainnet-real mode requires `--mainnet-allow-symbols` to be set; an empty value makes startup fatal. The wildcard `*` opts in to allow-all explicitly. `OrderOrCancel::Cancel` always passes through (emergency_stop must work).

**Architecture:** A new `IntentChecker` trait lives in `executor-hl::intent_checker` (shape: `fn check_place(&self, &OrderIntent) -> Result<(), String>`). `BatchSender` gains an optional `Option<Arc<dyn IntentChecker>>` field; the existing `spawn_batch_sender` becomes a thin wrapper that calls a new `spawn_batch_sender_with_gate`. The concrete `SafetyGate` struct lives in `executor-server::safety`, holds `Option<HashSet<Symbol>>` and `Option<Decimal>`, and implements `IntentChecker`. The startup path (`main.rs`) parses CLI flags via `clap`, builds the gate via `SafetyGate::from_args`, registers it on the `BatchSender` (only in real mode) and in `ServerState`. The `start_exec` route handler reads `book.best_bid` and calls `safety.check_request` before dispatching the algo.

**Tech Stack:** Rust 2021 (workspace MSRV 1.91), no new deps. Existing: `clap 4 derive+env`, `rust_decimal 1.x`, `thiserror 1.x`, `tokio 1.x`, `tower 0.5`. Tests use `mockito 1.7` (existing) for HTTP mocks, `tower::ServiceExt::oneshot` for axum integration.

---

## File Structure

| Path | Responsibility | Action |
|---|---|---|
| `executor/crates/executor-hl/src/intent_checker.rs` | NEW. Defines `IntentChecker` trait. ~10 LOC. | Create |
| `executor/crates/executor-hl/src/lib.rs` | Add `pub mod intent_checker;` and re-export `pub use intent_checker::IntentChecker;`. | Modify |
| `executor/crates/executor-hl/src/batch_sender.rs` | Add `gate: Option<Arc<dyn IntentChecker>>` field on `BatchSender`. New constructor `spawn_batch_sender_with_gate`. Existing `spawn_batch_sender` becomes a thin wrapper passing `gate=None`. `enqueue` and `enqueue_with_ack` consult the gate for `OrderOrCancel::Place` and reject violations with `HlError::ActionFormat`. Manual `Debug` impl for `BatchSender` (because `dyn IntentChecker: !Debug` by default). 3 new tests. | Modify |
| `executor/crates/executor-server/src/safety.rs` | NEW. `SafetyGate` struct, `SafetyViolation` enum, `from_args` constructor, `check_request`/`check_intent` methods, `impl IntentChecker for SafetyGate`. ~150 LOC + 8 unit tests. | Create |
| `executor/crates/executor-server/src/lib.rs` | Add `pub mod safety;` and re-export `pub use safety::{SafetyGate, SafetyViolation};`. | Modify |
| `executor/crates/executor-server/src/state.rs` | Add `pub safety: Arc<SafetyGate>` field to `ServerState`. Update `ServerState::new` signature to accept `safety: Arc<SafetyGate>`. | Modify |
| `executor/crates/executor-server/src/routes.rs` | In `start_exec`, after `validate_algorithm_name`, call `safety.check_request` using `book.best_bid` as `ref_px`. Return `400 Bad Request` on violation. | Modify |
| `executor/crates/executor-server/src/main.rs` | Add 2 new CLI flags. Construct `SafetyGate` via `from_args` (panic on mainnet-real with empty allow-list). Pass `Option<Arc<dyn IntentChecker>>` into `spawn_batch_sender_with_gate`. Pass `Arc<SafetyGate>` into `ServerState::new`. | Modify |
| `executor/crates/executor-server/tests/integration_rest.rs` | Update `build_state_with_seed` to pass `Arc::new(SafetyGate::disabled())` to `ServerState::new`. Add 3 new tests with non-disabled gates. | Modify |
| `docs/superpowers/specs/2026-05-05-pr-c2-symbol-allowlist-size-cap-design.md` | Already created — reference doc. | (already done) |
| `docs/HANDOFF-2026-05-05.md` | Append PR-C2 完了 subsection at session end. | Modify (last step) |

---

## Step 1: Add `IntentChecker` trait

- [ ] Create `executor/crates/executor-hl/src/intent_checker.rs` with:
  ```rust
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
  ```
- [ ] In `executor/crates/executor-hl/src/lib.rs`, add `pub mod intent_checker;` and `pub use intent_checker::IntentChecker;` (next to existing `pub mod batch_sender;`).
- [ ] Verify build: `cd executor && cargo build -p executor-hl`.

## Step 2: Wire gate into `BatchSender`

- [ ] In `executor/crates/executor-hl/src/batch_sender.rs`:
  - Import: `use crate::intent_checker::IntentChecker;`
  - Replace `#[derive(Debug, Clone)]` on `BatchSender` with manual `Debug` impl (because `Arc<dyn IntentChecker>` doesn't auto-derive Debug):
    ```rust
    #[derive(Clone)]
    pub struct BatchSender {
        tx: mpsc::Sender<Envelope>,
        gate: Option<Arc<dyn IntentChecker>>,
    }

    impl std::fmt::Debug for BatchSender {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BatchSender")
                .field("gate", &self.gate.is_some())
                .finish()
        }
    }
    ```
  - Modify `enqueue`:
    ```rust
    pub fn enqueue(&self, item: OrderOrCancel) -> Result<(), HlError> {
        if let Some(gate) = &self.gate {
            if let OrderOrCancel::Place(intent) = &item {
                if let Err(reason) = gate.check_place(intent) {
                    tracing::error!(
                        reason, cloid = ?intent.cloid, symbol = %intent.symbol,
                        "safety_gate: drop place"
                    );
                    return Err(HlError::ActionFormat(format!("safety_gate: {reason}")));
                }
            }
        }
        self.tx
            .try_send(Envelope { item, ack: None })
            .map_err(|e| HlError::Network(format!("batch enqueue: {e}")))
    }
    ```
  - Modify `enqueue_with_ack` similarly (gate check before sending the envelope; return `Err(HlError::ActionFormat)` so the caller's `oneshot::Receiver` resolves to that error in the same way as a flusher-side failure).
  - Add new function:
    ```rust
    pub fn spawn_batch_sender_with_gate<C>(
        client: Arc<C>,
        gate: Option<Arc<dyn IntentChecker>>,
        cfg: BatchSenderConfig,
    ) -> (BatchSender, BatchSenderHandle)
    where
        C: HlClient + 'static,
    {
        let (tx, rx) = mpsc::channel(1024);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(flusher_loop(client, rx, cfg, shutdown_rx));
        (
            BatchSender { tx, gate },
            BatchSenderHandle { join, shutdown: shutdown_tx },
        )
    }
    ```
  - Refactor existing `spawn_batch_sender` to call `spawn_batch_sender_with_gate(client, None, cfg)`.
- [ ] Verify build: `cargo build -p executor-hl`.

## Step 3: BatchSender unit tests for the gate

- [ ] In `executor/crates/executor-hl/src/batch_sender.rs::tests`, add 3 tests:
  - `enqueue_with_gate_rejects_violation`: define a mock `RejectAll` checker → expect `Err(HlError::ActionFormat(...))` from `enqueue`.
  - `enqueue_with_gate_passes_ok`: define a mock `AllowAll` checker → enqueue + flush + assert mock client received the order.
  - `enqueue_cancel_skips_gate`: gate present (`RejectAll`), enqueue `OrderOrCancel::Cancel` → must succeed.
  - Helper inside test mod:
    ```rust
    #[derive(Debug)]
    struct AllowAll;
    impl IntentChecker for AllowAll {
        fn check_place(&self, _: &OrderIntent) -> Result<(), String> { Ok(()) }
    }
    #[derive(Debug)]
    struct RejectAll;
    impl IntentChecker for RejectAll {
        fn check_place(&self, _: &OrderIntent) -> Result<(), String> {
            Err("blocked by test".into())
        }
    }
    ```
- [ ] Run: `cargo test -p executor-hl batch_sender`.

## Step 4: SafetyGate struct in executor-server

- [ ] Create `executor/crates/executor-server/src/safety.rs`:
  ```rust
  //! Mainnet safety gate: symbol allow-list + per-order USD notional cap.
  //!
  //! Wires into the BatchSender via the `IntentChecker` trait at startup.
  //! Mock mode constructs a `disabled()` gate which short-circuits all checks.
  //! Real mode requires `--mainnet-allow-symbols` to be set when targeting
  //! mainnet; an empty value makes startup fatal.

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
      /// Mock mode / explicit allow-all. Everything passes.
      pub fn disabled() -> Self {
          Self {
              allow_symbols: None,
              max_notional_usd: None,
          }
      }

      /// Build from CLI args.
      ///
      /// `is_mainnet_real = (mode==real && base==mainnet)`.
      /// When that's true, an empty `allow_csv` is fatal — Gemini deep review
      /// (2026-05-05): "warn ログは見落とされる. 金融系では明示 opt-in を要求する".
      ///
      /// `allow_csv = "*"` is the explicit opt-in to allow-all.
      /// `allow_csv = "ETH,BTC"` is the typical case.
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
              // Non-mainnet-real with empty CSV: keep as Some(empty) so testnet
              // gate testing has a meaningful "reject everything" config.
              Some(HashSet::new())
          } else if trimmed == "*" {
              None
          } else {
              Some(
                  trimmed
                      .split(',')
                      .map(|s| Symbol::new(s.trim().to_string()))
                      .collect(),
              )
          };
          Ok(Self {
              allow_symbols,
              max_notional_usd: max_usd.map(Decimal::from),
          })
      }

      /// Layer 1: REST-entry rough check using a reference price (best_bid).
      /// `ref_px = None` skips the notional check.
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

  // ---- tests ----
  #[cfg(test)]
  mod tests {
      // 8 tests as listed in spec §5.1
  }
  ```
- [ ] In `executor/crates/executor-server/src/lib.rs`, add `pub mod safety;` and `pub use safety::{SafetyGate, SafetyViolation};`.
- [ ] `executor/crates/executor-server/Cargo.toml` には既存 `executor-core` `executor-hl` `rust_decimal` `thiserror` があるはず — 確認のみ. なければ追加.
- [ ] Run: `cargo build -p executor-server`.

## Step 5: SafetyGate unit tests (8 tests)

- [ ] Add the test module body in `safety.rs` covering spec §5.1:
  1. `disabled_passes_request`: `disabled().check_request(&Symbol::new("XYZ"), dec!(1), Some(dec!(99999)))` → `Ok(())`.
  2. `disabled_passes_intent`: `disabled().check_intent(&order_intent)` → `Ok(())`.
  3. `allow_list_rejects_non_member`: `from_args("ETH,BTC", None, false)` then `check_request(Symbol("XRP"), ...)` → `SymbolNotAllowed`.
  4. `allow_list_accepts_member`: same gate, `check_request(Symbol("ETH"), dec!(0.001), Some(dec!(2400)))` → `Ok`.
  5. `notional_cap_rejects_over`: `from_args("ETH", Some(10), false)` then `check_request(Symbol("ETH"), dec!(0.005), Some(dec!(2400)))` → notional=12 > 10 → `NotionalExceeded`.
  6. `notional_cap_accepts_under`: same gate, `check_request(Symbol("ETH"), dec!(0.004), Some(dec!(2400)))` → notional=9.6 ≤ 10 → `Ok`.
  7. `from_args_mainnet_real_empty_fatal`: `from_args("", None, true)` → `Err`. Assert error message contains `"required"`.
  8. `from_args_star_means_allow_all`: `from_args("*", None, true)` → `Ok` and `gate.allow_symbols.is_none()`.
  Plus 1 extra:
  9. `from_args_csv_with_whitespace`: `from_args("ETH , BTC ,LINK", None, false)` → `Ok`, all 3 symbols normalized into the set.
- [ ] Run: `cargo test -p executor-server safety`.

## Step 6: Update `ServerState` to carry the gate

- [ ] In `executor/crates/executor-server/src/state.rs`:
  - Add field `pub safety: Arc<SafetyGate>` to `ServerState`.
  - Update `ServerState::new` signature: append `safety: Arc<SafetyGate>` parameter.
  - Update `Debug` impl: include `safety` (it derives `Debug`).
- [ ] All call sites of `ServerState::new` will fail to compile until updated. Note them (main.rs + integration_rest.rs).

## Step 7: Update `routes.rs::start_exec` for Layer 1

- [ ] In `executor/crates/executor-server/src/routes.rs::start_exec`, after `validate_algorithm_name(&req.algorithm)?;`:
  ```rust
  let symbol = Symbol::new(req.symbol.clone());
  let ref_px = {
      let book_g = s.app_state.book.read().await;
      book_g.get(&symbol).and_then(|b| b.best_bid())
  };
  s.safety
      .check_request(&symbol, req.target_size, ref_px)
      .map_err(|v| ServerError::BadRequest(format!("safety_gate: {v}")))?;
  ```
  - Note: `req.symbol` is moved later into `Symbol::new(req.symbol)` for `ctx.symbol`. Refactor so `let symbol = Symbol::new(req.symbol.clone());` happens once at the top, then reuse it for `ctx.symbol = symbol`.
- [ ] Run: `cargo build -p executor-server` (will fail at `ServerState::new` call sites — fix in next step).

## Step 8: Update `main.rs`

- [ ] In `executor/crates/executor-server/src/main.rs`:
  - Add to `Args`:
    ```rust
    /// Mainnet allow-list of symbols (comma-separated, e.g. "ETH,BTC").
    /// REQUIRED for `--mode real --base mainnet`.
    /// Use `*` to explicitly allow all (NOT recommended for production).
    #[arg(long, env = "EXECUTOR_MAINNET_ALLOW_SYMBOLS", default_value = "")]
    mainnet_allow_symbols: String,

    /// Per-order USD notional cap. Omit for no cap (NOT recommended for production).
    #[arg(long, env = "EXECUTOR_MAINNET_MAX_NOTIONAL_USD")]
    mainnet_max_notional_usd: Option<u64>,
    ```
  - In `main()`, immediately after `let args = Args::parse();` and before `app_state` construction:
    ```rust
    let is_mainnet_real = matches!(args.mode, Mode::Real) && matches!(args.base, Base::Mainnet);
    let safety = Arc::new(match args.mode {
        Mode::Mock => executor_server::SafetyGate::disabled(),
        Mode::Real => executor_server::SafetyGate::from_args(
            &args.mainnet_allow_symbols,
            args.mainnet_max_notional_usd,
            is_mainnet_real,
        ).context("safety gate construction failed")?,
    });
    tracing::info!(
        allow_symbols = ?safety.allow_symbols,
        max_notional_usd = ?safety.max_notional_usd,
        "safety gate constructed",
    );
    ```
  - Replace both `spawn_batch_sender(...)` calls with `spawn_batch_sender_with_gate`:
    ```rust
    let gate_dyn: Option<Arc<dyn IntentChecker>> = match args.mode {
        Mode::Mock => None,
        Mode::Real => Some(safety.clone() as Arc<dyn IntentChecker>),
    };
    // (inside Mode::Mock match arm)
    let (batch_sender, batch_handle) =
        spawn_batch_sender_with_gate(mock_hl.clone(), None, batch_cfg);
    // (inside Mode::Real match arm)
    let (batch_sender, batch_handle) =
        spawn_batch_sender_with_gate(real_client.clone(), gate_dyn.clone(), batch_cfg);
    ```
    - Note: `gate_dyn` is computed once before the match. `Mode::Mock` arm passes `None` literal (the match is exhaustive on mode anyway). Choose: simpler is to keep `gate_dyn` computed and pass it into both arms — `None` in the mock arm, `Some(...)` in the real arm. We can also avoid hoisting `gate_dyn` and just inline `match`.
  - Update import: `use executor_hl::batch_sender::{spawn_batch_sender_with_gate, BatchSenderConfig};` (drop `spawn_batch_sender` since main no longer uses the no-gate variant).
  - Add import: `use executor_hl::IntentChecker;`.
  - Update `ServerState::new(...)` to include `safety.clone()` as the last argument.
- [ ] Run: `cd executor && cargo build -p executor-server`. Should compile clean.

## Step 9: Update integration tests

- [ ] In `executor/crates/executor-server/tests/integration_rest.rs`:
  - Add import: `use executor_server::SafetyGate;`.
  - Modify `build_state_with_seed`:
    - Append: `let safety = Arc::new(SafetyGate::disabled());`.
    - Pass `safety` as the 6th argument to `ServerState::new(...)`.
  - Add helper `build_state_with_safety(safety: SafetyGate) -> (Arc<ServerState>, Arc<MockHlClient>)` that does the same setup but takes a custom gate.
  - Add 3 new tests:
    1. `start_exec_symbol_not_allowed_400`:
       ```rust
       let safety = SafetyGate::from_args("BTC", None, false).unwrap();
       let (state, _) = build_state_with_safety(safety).await;
       // POST to /v1/exec with symbol="ETH" → expect 400 + body contains "safety_gate"
       ```
    2. `start_exec_notional_exceeded_400`:
       ```rust
       // Seed BTC book at $50000; gate allow=BTC max=$10
       // Request target_size=0.001 (= $50 notional) → 400
       ```
    3. `start_exec_within_caps_200`:
       ```rust
       // Same setup; request target_size=0.0001 (= $5) → 200
       ```
- [ ] Run: `cargo test -p executor-server`.

## Step 10: Workspace-wide checks

- [ ] `cd executor && cargo fmt --all`.
- [ ] `cargo build --workspace`.
- [ ] `cargo test --workspace` — expect ~158-160 tests pass (existing 145 + new ~14).
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` — must be clean.
- [ ] `cd .. && bash scripts/check_ci_local.sh` (local CI mirror).

## Step 11: Live validation prep (offline check, no PK access)

- [ ] Verify the new CLI:
  ```bash
  cd executor
  # Should show new flags in --help
  cargo run -p executor-server -- --help
  ```
- [ ] (For user, separate terminal — Claude does NOT run this:)
  ```bash
  # Sanity: empty allow-list on mainnet+real → fatal
  cargo run -p executor-server -- --mode real --base mainnet
  # Expect: Error: --mainnet-allow-symbols is required ...

  # Sanity: real mode with valid gate → starts
  source scripts/load-env.sh
  cargo run -p executor-server -- --mode real --base mainnet \
    --mainnet-allow-symbols ETH \
    --mainnet-max-notional-usd 20
  # Expect: log "safety gate constructed allow_symbols=Some({Symbol(\"ETH\")}) max_notional_usd=Some(20)"
  ```
- [ ] (Same, when user has time:) curl tests against the live `--mode real` server:
  - `curl -XPOST localhost:8085/v1/exec -d '{"algorithm":"market","symbol":"BTC","intent":"open","target_size":"0.0001"}'` → 400
  - `curl -XPOST localhost:8085/v1/exec -d '{"algorithm":"market","symbol":"ETH","intent":"open","target_size":"5"}'` → 400 (notional > 20)
  - `curl -XPOST localhost:8085/v1/exec -d '{"algorithm":"market","symbol":"ETH","intent":"open","target_size":"0.001"}'` → 200

## Step 12: Doc updates and commit

- [ ] Append to `docs/HANDOFF-2026-05-05.md` a `## 10. PR-C2 完了` subsection summarizing what shipped, key Gemini-flipped decisions (Q3 fatal, Q4 Option), and the next step (PR-C3).
- [ ] Commit:
  ```bash
  git add docs/superpowers/specs/2026-05-05-pr-c2-symbol-allowlist-size-cap-design.md
  git add docs/superpowers/plans/2026-05-05-pr-c2-symbol-allowlist-size-cap-plan.md
  git commit -m "docs(spec/plan): PR-C2 mainnet safety gate (allowlist + size cap) design + plan"

  git add executor/crates/executor-hl/src/intent_checker.rs
  git add executor/crates/executor-hl/src/lib.rs
  git add executor/crates/executor-hl/src/batch_sender.rs
  git add executor/crates/executor-server/src/safety.rs
  git add executor/crates/executor-server/src/lib.rs
  git add executor/crates/executor-server/src/state.rs
  git add executor/crates/executor-server/src/routes.rs
  git add executor/crates/executor-server/src/main.rs
  git add executor/crates/executor-server/tests/integration_rest.rs
  git commit -m "feat(executor): PR-C2 mainnet safety gate (allowlist + size cap)"

  git add docs/HANDOFF-2026-05-05.md
  git commit -m "docs: HANDOFF — PR-C2 完了"
  ```
- [ ] Push to develop or branch + PR (per branch strategy: pre-v1.0 direct push to develop is OK, but feature work for safety-critical change merits a PR for visibility).

## Step 13: PR creation (--base develop)

- [ ] `git push origin develop` (or feature branch).
- [ ] If feature branch: `gh pr create --base develop --title "feat(executor): PR-C2 mainnet safety gate (allowlist + size cap)" --body ...`.
- [ ] Wait for CI green; user reviews and merges.

---

## Acceptance gates

- [ ] All tests pass: `cargo test --workspace`
- [ ] No clippy warnings: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] CI script clean: `scripts/check_ci_local.sh`
- [ ] `--mode real --base mainnet` with empty `--mainnet-allow-symbols` returns nonzero exit + clear error message (manual verification by user)
- [ ] `--mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 20` starts cleanly with safety gate logged
- [ ] PR description clearly explains the 2-layer design, Gemini-flipped decisions, and the production deployment intent
