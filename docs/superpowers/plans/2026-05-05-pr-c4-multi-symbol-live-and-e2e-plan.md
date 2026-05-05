# PR-C4 Implementation Plan: multi-symbol live test + Python e2e CI + operator_id

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans.

**Goal:** Land Phase 3.5's final piece. (1) Add `operator_id` support to the Python `ExecutorClient` so every POST audit-tags the caller. (2) Lift the existing Rust-binary-driven Python e2e tests out of the `live` marker and into CI; CI builds `executor-server --release` once and runs `pytest -m "not live"` (which now picks them up). (3) Drop a placeholder testnet multi-symbol live test under `#[cfg(feature = "live")]` for the user to drive manually.

**Architecture:** Three independent pieces.
- *Python connector*: `ExecutorClient.__init__` gains `operator_id: str | None`, `_post` injects `X-Operator-ID` when set; mock-transport tests verify both branches.
- *Rust server audit*: `routes::start_exec` and `routes::cancel_exec` accept the `HeaderMap` extractor and emit `tracing::info!(operator, …)` so the audit chain is uniform across endpoints.
- *Testing*: `running_server` fixture switches from `module` to `function` scope (PR-C3 emergency_stop mutates global state), `@pytest.mark.live` is dropped from existing tests, three new e2e tests cover idempotency + 503 + operator_id round-trip. `scripts/check_ci_local.sh` and the GitHub Actions workflow get a `cargo build --release -p executor-server` step before pytest.

**Tech Stack:** Python 3.12 + httpx + pytest + pytest-asyncio (existing). Rust 2021 (existing). No new deps.

---

## File Structure

| Path | Action | Notes |
|---|---|---|
| `src/executor/client.py` | Modify | Add `operator_id` parameter; `_post` injects `X-Operator-ID` header. ~20 LOC delta. |
| `tests/test_executor_client.py` | Modify | 2 new mock-transport tests for operator_id. |
| `executor/crates/executor-server/src/routes.rs` | Modify | `start_exec` and `cancel_exec` accept `HeaderMap` and log `operator`. |
| `tests/test_executor_client_live.py` | Modify | Drop `@pytest.mark.live` from existing tests. Switch fixture to function scope. Add 3 e2e tests. |
| `scripts/check_ci_local.sh` | Modify | Add `cargo build --release -p executor-server` step. Update pytest filter to `-m "not live"`. |
| `.github/workflows/*.yml` | Modify (if present) | Same updates: build + drop `slow` from filter. |
| `executor/crates/executor-hl/tests/live_emergency_stop_multi_testnet.rs` | Create | Placeholder live test for testnet multi-symbol cancel; gated by `#[cfg(feature = "live")]`. ~150 LOC. |
| `scripts/load-env-testnet.sh` | Create | Template for users to source testnet PK from `~/.password-store/diff-old-new/hl-testnet/agent-pk`. |
| `docs/HANDOFF-2026-05-05.md` | Modify | Append §12 PR-C4 完了 + Phase 3.5 完成宣言. |

---

## Step 1: Python connector `operator_id`

- [ ] Edit `src/executor/client.py::ExecutorClient`:
  - Add parameter: `def __init__(self, base_url, operator_id=None, timeout=10.0)`.
  - Store `self._operator_id = operator_id`.
  - Update `_post(self, path, payload)` to merge `{"X-Operator-ID": self._operator_id}` into headers when set.
  - Don't touch `_get` (audit POSTs only).
- [ ] Run: `python -m pytest tests/test_executor_client.py -q`.

## Step 2: Mock-transport tests for operator_id

- [ ] In `tests/test_executor_client.py`, add 2 tests:
  ```python
  @pytest.mark.asyncio
  async def test_post_includes_operator_id_when_set(monkeypatch):
      seen_headers: dict = {}
      def handler(req):
          seen_headers.update(dict(req.headers))
          return httpx.Response(200, json={"aborted_executions": 0, "cancelled_orders": 0})
      cli = _patched_client(monkeypatch, handler)
      cli._operator_id = "alice@desk"  # or pass via __init__ if patched_client accepts
      async with cli:
          await cli.emergency_stop()
      assert seen_headers.get("x-operator-id") == "alice@desk"

  @pytest.mark.asyncio
  async def test_post_omits_operator_id_when_none(monkeypatch):
      ...  # symmetric: assert "x-operator-id" not in seen_headers
  ```
  - Adjust `_patched_client` helper to accept `operator_id` kwarg if needed.

## Step 3: Server-side audit log

- [ ] In `executor/crates/executor-server/src/routes.rs::start_exec`:
  - Change signature to accept `headers: axum::http::HeaderMap`.
  - Read `operator` (default `"unknown"`) and emit `tracing::info!(operator, algorithm = %req.algorithm, symbol = %req.symbol, "start_exec")` after the safety-gate check.
- [ ] Same change in `cancel_exec` (no operator yet, add it).
- [ ] Run: `cd executor && cargo build -p executor-server`.

## Step 4: Update existing Python e2e tests

- [ ] In `tests/test_executor_client_live.py`:
  - Remove `@pytest.mark.live` from every existing `test_e2e_*` function. Keep `@pytest.mark.slow`.
  - Change `running_server` fixture to `scope="function"` (PR-C3 emergency_stop mutates `shutdown_initiated`).
  - Update the docstring to reflect the new CI inclusion.

## Step 5: New Python e2e tests for PR-C3 features

- [ ] In `tests/test_executor_client_live.py`, add:
  ```python
  @pytest.mark.slow
  @pytest.mark.asyncio
  async def test_e2e_emergency_stop_with_operator_id(running_server: str) -> None:
      async with ExecutorClient(running_server, operator_id="alice@desk") as cli:
          body = await cli.emergency_stop()
      assert body["aborted_executions"] == 0

  @pytest.mark.slow
  @pytest.mark.asyncio
  async def test_e2e_idempotent_emergency_stop(running_server: str) -> None:
      async with ExecutorClient(running_server) as cli:
          first = await cli.emergency_stop()
          second = await cli.emergency_stop()
      assert second["aborted_executions"] == 0
      assert second["cancelled_orders"] == 0

  @pytest.mark.slow
  @pytest.mark.asyncio
  async def test_e2e_503_after_emergency_stop(running_server: str) -> None:
      async with ExecutorClient(running_server) as cli:
          await cli.emergency_stop()
          with pytest.raises(Exception, match="HTTP 503"):
              await cli.start(
                  algorithm="market", symbol="BTC", intent="open", target_size="0.001",
              )
  ```

## Step 6: CI script update

- [ ] Edit `scripts/check_ci_local.sh`:
  - Before `[python 4/4] Pytest`, add a `[rust 0/3] cargo build --release -p executor-server` step.
  - Change pytest filter from `-m "not live and not slow"` to `-m "not live"`.
- [ ] Inspect `.github/workflows/*.yml` (if present) and apply the same change.

## Step 7: Workspace + Python checks

- [ ] `cd executor && cargo fmt --all && cargo build --workspace && cargo test --workspace`.
- [ ] `cd .. && .venv/bin/pytest -m "not live"` — expect new e2e tests to pass.
- [ ] `bash scripts/check_ci_local.sh` — green.

## Step 8: testnet live test placeholder

- [ ] Create `executor/crates/executor-hl/tests/live_emergency_stop_multi_testnet.rs`:
  ```rust
  #![cfg(feature = "live")]
  // ENV:
  //   HL_TESTNET_AGENT_PK — testnet 1-shot agent wallet PK (from pass-store)
  //   HL_TESTNET_MASTER   — testnet master EOA (public address)
  //
  // Run:
  //   source scripts/load-env-testnet.sh
  //   cd executor
  //   cargo test -p executor-hl --features live live_testnet_multi_cancel \
  //     -- --nocapture --test-threads=1
  //
  // 2 件並行 place (ETH + BTC, ALO post-only, $11 notional each) →
  // cancel_orders(&[c1, c2]) で両方 cancelled. existing positions unchanged.
  ```
  - 同じパターンを `live_mainnet_place_cancel.rs` から流用. mainnet → testnet config に切替.
  - 2 symbol 同時 place → cancel_orders(&[c1, c2]) → 両方 cancelled assert.
  - `MetaCache::build` で testnet meta 取得.
  - Optional: 起動前 baseline snapshot 取得, 終了時 unchanged 検証.

## Step 9: load-env-testnet.sh template

- [ ] Create `scripts/load-env-testnet.sh`:
  ```bash
  #!/usr/bin/env bash
  # Source-only. Loads testnet PK + master into the calling shell.
  # Mirrors load-env.sh but reads from a separate pass-store entry so
  # mainnet and testnet keys never share a process.
  set -e
  if ! [[ "$_" =~ "load-env-testnet.sh" || -n "${BASH_SOURCE[0]}" ]]; then
      echo "must be sourced, not executed"
      exit 1
  fi
  export HL_TESTNET_AGENT_PK="$(pass show diff-old-new/hl-testnet/agent-pk 2>/dev/null || true)"
  if [[ -z "$HL_TESTNET_AGENT_PK" ]]; then
      echo "WARN: no testnet agent-pk in pass-store. Set with: pass insert diff-old-new/hl-testnet/agent-pk"
  fi
  # Public addresses are not secret; load from .env.testnet if available.
  if [[ -f .env.testnet ]]; then
      set -a
      source .env.testnet
      set +a
  fi
  ```
  - Note: `.env.testnet` itself is git-ignored (HL_TESTNET_MASTER + agent-related metadata).
  - Document pass-store key path in `CLAUDE.md` (project) — reuse the existing PK-protection rules.

## Step 10: HANDOFF doc + Phase 3.5 完成宣言

- [ ] Append `## 12. PR-C4 完了 + Phase 3.5 完成宣言` to `docs/HANDOFF-2026-05-05.md` listing all PRs from PR-A through PR-C4 and stating the system is production-ready (mainnet small-size tested, mock e2e covered in CI).

## Step 11: Commit + push + PR

- [ ] Branch: `git checkout -b feat/pr-c4-multi-symbol-and-e2e`.
- [ ] Commits split:
  1. `docs(spec/plan): PR-C4 multi-symbol live + Python e2e + operator_id`
  2. `feat(client): operator_id passthrough on POST`
  3. `feat(executor): start_exec/cancel_exec audit log`
  4. `test(e2e): drop live marker + add PR-C3 idempotency e2e`
  5. `chore(ci): build executor-server release before pytest`
  6. `test(executor-hl): live_emergency_stop_multi_testnet placeholder`
  7. `chore(env): load-env-testnet.sh template`
  8. `docs: HANDOFF — PR-C4 完了 + Phase 3.5 完成宣言`
- [ ] Push + `gh pr create --base develop`.
- [ ] CI green wait + self-merge per branch strategy.

---

## Acceptance gates

- [ ] All Rust + Python tests green.
- [ ] CI runs the e2e tests (test names visible in CI log).
- [ ] `start_exec` and `cancel_exec` log lines include the operator id.
- [ ] testnet live test compiles under `--features live` (cannot run without PK).
- [ ] HANDOFF marks Phase 3.5 complete.
