# PR-D1 Implementation Plan: WS subscriber + REST polling fallback

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans.

**Goal:** Make `executor-server` automatically establish a WebSocket subscription to Hyperliquid on startup (real mode), receive `userFills` / `orderUpdates` / `l2Book` events, and feed them into `AppState` via the existing `WsStateManager`. Add a REST-poll fallback for the agent's userFills so that **every algorithm (PASSIVE_FOLLOW in particular) keeps making progress even while the WS is disconnected**. Reconcile state on reconnect and on a 5-minute cadence.

**Architecture:** Three concerns, three modules.

1. **Wire decoding** (`executor-hl::wire::ws`): one `WsFrame` enum decoded directly from HL's JSON envelope (`{channel, data}`). Includes pong + subscriptionResponse variants that we discard. Decoded frames fan out into the existing `WsMessage` enum (`L2Book / UserFill / UserPosition / OrderUpdate`).

2. **Subscriber loop** (`executor-hl::ws_subscriber`): one `tokio::spawn`'d supervisor that owns the connection lifecycle. Inside a single connection, it spawns three concurrent sub-tasks: a read loop, a watchdog, and a REST fallback poller; on disconnect it joins them all, increments a reconnect counter, sleeps an exponential backoff, and tries again. Reconcile (REST snapshot of open_orders + positions + recent fills) runs once on every successful (re)connection AND once every 5 min.

3. **Wire-up** (`executor-server::main`): real mode constructs a `WsSubscriberConfig` from CLI args + safety gate, calls `spawn_ws_subscriber`, and stows the resulting handle on `ServerState`. Mock mode skips the subscriber entirely. Health endpoint reads the live `WsStatus` and reports it.

Two cross-cutting concerns:

- **Fill dedup**: the WS feed and the REST fallback can both deliver the same fill. Dedup keys on `(oid, tid)` (HL's `tid` = trade id). Implemented inside `WsStateManager::apply_fill` with a `HashSet<(OrderId, u64)>` guarded by a `RwLock`. Old entries are pruned when the underlying `recent_fills` ring is pruned, so the dedup set stays bounded.
- **State reconciliation**: WS reconnection loses ordering of events that fired during the gap. On every `(re)connect` we synchronously snapshot REST `fetch_open_orders` + `fetch_account_state` and overwrite `AppState.open_orders` / `AppState.position` under brief write locks. The 5-min reconcile loop does the same.

**Tech Stack:** Rust 2021 (workspace MSRV 1.91). Existing deps: `tokio-tungstenite 0.29` (workspace), `serde`, `serde_json`. New deps: none.

---

## Subagent guidance

This plan is large (~12 steps, multiple new modules, async lifetime juggling). **Recommended split into three subagent dispatches**:

- **Subagent A — wire decode**: Steps 1, 2, 3. Pure parsing, no networking. Output: `wire::ws` module + dedup field on `WsFill` + tests.
- **Subagent B — subscriber loop**: Steps 4, 5, 6, 7. Async, networking, supervision. Output: `ws_subscriber` module + REST fallback + reconcile + tests using a fake WS server.
- **Subagent C — wire-up + smoke**: Steps 8, 9, 10, 11, 12. Hooks into `ServerState`, `main.rs`, health endpoint, integration test, docs, commit. Verify mainnet smoke with the user.

Each subagent should run `cargo build && cargo test -p executor-hl` (A,B) or `cargo test --workspace` (C) at the end of its block before yielding control.

---

## File Structure

| Path | Action | Notes |
|---|---|---|
| `executor/crates/executor-hl/src/wire/ws.rs` | Create | `WsFrame` + decode targets (l2Book, userFills, orderUpdates, pong, subscriptionResponse). ~200 LOC + decoder tests. |
| `executor/crates/executor-hl/src/wire/mod.rs` | Modify | Add `pub mod ws;`. |
| `executor/crates/executor-hl/src/ws_state.rs` | Modify | Add `tid: u64` to `WsFill`. Add `seen_trade_ids: RwLock<HashSet<(OrderId, u64)>>` field on `WsStateManager`. Dedup in `apply_fill`. |
| `executor/crates/executor-hl/src/ws_subscriber.rs` | Create | `WsSubscriberConfig`, `WsSubscriberHandle`, `WsStatus`, `spawn_ws_subscriber`, internal supervisor + read + watchdog + REST fallback + 5-min reconcile sub-tasks. ~400 LOC + tests. |
| `executor/crates/executor-hl/src/lib.rs` | Modify | `pub mod ws_subscriber;`, `pub use ws_subscriber::{...}`. |
| `executor/crates/executor-hl/src/hl_client.rs` | Modify | Add `fetch_user_fills_by_time(agent, start_ms) -> Vec<WireUserFill>` to `HlClient` trait + `MockHlClient` + `RealHlClient`. Used by REST fallback. |
| `executor/crates/executor-server/src/state.rs` | Modify | Add `pub ws_handle: Option<Arc<WsSubscriberHandle>>` and `pub ws_status: Arc<WsStatus>` (mock = sentinel `WsStatus::disabled()`). Drop sends shutdown. |
| `executor/crates/executor-server/src/main.rs` | Modify | Real mode constructs `WsSubscriberConfig` from CLI args + safety, calls `spawn_ws_subscriber`, stores handle on state. Mock mode skips. |
| `executor/crates/executor-server/src/routes.rs` | Modify | `health` route reads `ws_status` and surfaces ws fields in response. |
| `docs/HANDOFF-2026-05-05.md` | Modify | Append §13 PR-D1 完了 + Phase 4 開始宣言. |

---

## Step 1: Add `tid: u64` to `WsFill` + dedup field on `WsStateManager`

- [ ] Edit `executor-hl/src/ws_state.rs`:
  - Add `pub tid: u64` to `WsFill`.
  - Add field `seen_trade_ids: RwLock<HashSet<(OrderId, u64)>>` to `WsStateManager` (init empty in `new`).
  - Cap dedup set size: when it reaches 4096 entries, drain the oldest 1024. Implement as a simple counter + `clear()` is too aggressive; instead use a `VecDeque<(OrderId, u64)>` for FIFO eviction alongside the `HashSet`. Alternative: keep set unbounded and accept memory growth as proportional to fills (cheap; bounded by trading volume × runtime). Pick the simpler approach for v1: **`HashSet`, no eviction**, document as TODO if memory becomes an issue.
  - In `apply_fill`, before recording the fill, lock `seen_trade_ids` for write, attempt `insert((oid, tid))`. If insert returns `false`, the fill is a duplicate — return early without modifying `recent_fills` or `open_orders`.
  - Update existing unit tests for `apply_fill_*` to set `tid` (use any non-zero value).
- [ ] Run: `cargo test -p executor-hl ws_state`.

## Step 2: HL WS wire decoder (`executor-hl::wire::ws`)

- [ ] Create `executor-hl/src/wire/ws.rs`:
  - `pub enum WsFrame` with `#[serde(tag = "channel", rename_all = "camelCase")]` covering:
    - `Pong` (HL replies `{"channel":"pong"}` to app-level pings)
    - `SubscriptionResponse(serde_json::Value)` (subscribe ack — discard)
    - `L2Book { data: WireL2BookData }`
    - `UserFills { data: WireUserFillsData }` (HL sends a payload with `isSnapshot` boolean and `fills: [...]`)
    - `OrderUpdates { data: Vec<WireOrderUpdate> }` (HL sends a list directly)
  - `WireL2BookData { coin: String, levels: [Vec<WireBookLevel>; 2] }` — HL sends levels as `[bids, asks]`. Field `time` exists on the wire; ignore.
  - `WireBookLevel { px: Decimal (string), sz: Decimal (string), n: u32 }`. Use `serde_with::DisplayFromStr` or `Decimal`'s string deserializer.
  - `WireUserFillsData { is_snapshot: bool, user: String, fills: Vec<WireUserFill> }` (`user` is the address; checked but otherwise ignored).
  - `WireUserFill { coin: String, side: WireSide ("B"/"A"), px, sz, fee, oid: u64, tid: u64, cloid: Option<Cloid>, time: u64 }`.
  - `WireOrderUpdate { order: WireOrderState, status: WireOrderStatus, status_timestamp: u64 }`.
  - `WireOrderState { coin: String, side: WireSide, sz: Decimal, oid: u64, cloid: Option<Cloid>, ... }`.
  - Provide `impl From<WireUserFill> for WsFill` and `impl From<WireOrderUpdate> for Vec<WsOrderUpdate>` so the decoder can fan out into existing `WsMessage` variants.
  - **Decoder entry point**: `pub fn decode_frame(text: &str) -> Result<Option<Vec<WsMessage>>, serde_json::Error>` — returns `Ok(None)` for ack/pong, `Ok(Some(msgs))` for content, `Err` for parse failure.

- [ ] Add 8 unit tests covering:
  1. l2Book frame decode → 1 `WsMessage::L2Book`
  2. userFills snapshot=true → N `WsMessage::UserFill`
  3. userFills snapshot=false → N `WsMessage::UserFill`
  4. orderUpdates → N `WsMessage::OrderUpdate` for each item
  5. orderUpdates with `partiallyFilled` status → `WsOrderStatus::PartiallyFilled` with `remaining_sz`
  6. subscriptionResponse → `Ok(None)`
  7. pong → `Ok(None)`
  8. invalid JSON → `Err`

  Use HL's actual frame shapes from public docs as fixtures. If shapes are uncertain, add a comment marking the test as needing adjustment after first live run.

- [ ] Run: `cargo test -p executor-hl wire::ws`.

## Step 3: Add `fetch_user_fills_by_time` to `HlClient`

- [ ] Edit `executor-hl/src/hl_client.rs`:
  - Add to `HlClient` trait:
    ```rust
    async fn fetch_user_fills_by_time(
        &self,
        agent: &Address,
        start_ms: u64,
        end_ms: Option<u64>,
    ) -> Result<Vec<crate::wire::ws::WireUserFill>, HlError>;
    ```
  - `MockHlClient`: return `Ok(Vec::new())` (no fills replay). Optional: `pub seeded_fills_for_fallback: Mutex<Vec<...>>` for tests.
  - `RealHlClient`: POST `/info` with `{"type":"userFillsByTime","user":"0x...","startTime":N,"endTime":N|null}`. Parse response array. Use existing rate limiter + auth-free path.
- [ ] Add 1 mockito test for `RealHlClient::fetch_user_fills_by_time`.
- [ ] Run: `cargo test -p executor-hl fetch_user_fills`.

## Step 4: `WsSubscriberConfig` + `WsStatus` + `WsSubscriberHandle` skeleton

- [ ] Create `executor-hl/src/ws_subscriber.rs` with the public types only (no spawn yet). Document that `spawn_ws_subscriber` arrives in a later step.
  - `WsSubscriberConfig`: url, agent_address, master_address (for reconcile), symbols, durations.
  - `WsStatus`: connected (AtomicBool), last_message_at (RwLock<Option<DateTime>>), message_count (AtomicU64), reconnect_count (AtomicU32), last_error (RwLock<Option<String>>).
  - `WsStatus::disabled()` constructor for mock mode (connected=false but no error reporting).
  - `WsSubscriberHandle { join: JoinHandle<()>, shutdown: Arc<AtomicBool>, status: Arc<WsStatus> }`. `Drop` sets shutdown=true (best effort; the supervisor checks the flag between operations).
- [ ] Add to `executor-hl/src/lib.rs`: `pub mod ws_subscriber;` + re-exports.

## Step 5: Subscriber supervisor loop (no REST fallback yet)

- [ ] In `ws_subscriber.rs`, implement `pub fn spawn_ws_subscriber(cfg, manager, rest_client) -> WsSubscriberHandle`.
- [ ] Supervisor loop pseudocode:
  ```text
  loop:
    if shutdown.load() { break }
    status.connected = false
    match tokio_tungstenite::connect_async(&cfg.url).await {
      Err(e) => { record_error(e); backoff_sleep(); status.reconnect_count++; continue }
      Ok((ws, _)) => {
        // 1. subscribe
        ws.send(json subscribe userFills, agent).await?
        ws.send(json subscribe orderUpdates, agent).await?
        for sym in cfg.symbols: ws.send(json subscribe l2Book, sym).await?
        // 2. reconcile (REST snapshot)
        reconcile_once(rest_client, manager, cfg).await
        // 3. running state
        status.connected = true
        // 4. read loop until error / shutdown / stale
        run_connection(ws, manager, status, shutdown).await
      }
    }
    // disconnected; loop continues with backoff
  ```
- [ ] `run_connection`:
  ```text
  ticker_watchdog = interval(stale_after)
  ticker_reconcile = interval(reconcile_interval)
  ticker_ping = interval(20s)  // app-level ping (HL: send {"method":"ping"})
  loop:
    select! {
      Some(msg) = ws.next() => match msg {
        Text(t) => match decode_frame(&t) {
          Ok(Some(msgs)) => for m in msgs { manager.apply(m).await }
          Ok(None) => {} // pong/ack
          Err(e) => warn
        }
        Close => break (return)
        Ping(p) => ws.send(Pong(p)).await
        _ => {}
      }
      _ = ticker_watchdog.tick() => {
        if last_message_at older than stale_after → break (force reconnect)
      }
      _ = ticker_reconcile.tick() => reconcile_once(...)
      _ = ticker_ping.tick() => ws.send(Text(`{"method":"ping"}`)).await
      _ = shutdown_signal => break
    }
  ```
- [ ] `reconcile_once(rest, manager, cfg)`:
  - `let oo = rest.fetch_open_orders(&master, None).await?;`
  - `let acct = rest.fetch_account_state(&master, None).await?;`
  - Lock `state.open_orders` write, clear, repopulate from `oo` (cloid is missing on REST openOrders — keep oid-only entries with `cloid=Cloid::zero` placeholder; algos that care should already have their own cloid in `current_quote` so this is observational only).
  - Lock `state.position` write, overwrite from `acct.positions`.
  - update health.last_reconciliation = Now.
- [ ] Tests: integration test using a fake WS server (`tokio::net::TcpListener` + handcrafted JSON exchange). Verify the supervisor:
  1. connects, sends subscribe frames in correct order
  2. propagates a userFills frame into `recent_fills`
  3. survives a server-side close and reconnects (counter increments)
  4. exits cleanly when shutdown is set
- [ ] Run: `cargo test -p executor-hl ws_subscriber`.

## Step 6: REST polling fallback

- [ ] Inside the supervisor (single task), add a fourth select arm:
  ```text
  ticker_rest = interval(rest_poll_interval)  // 10s
  ...
  _ = ticker_rest.tick() => {
    if !status.connected || stale {
      let since = last_seen_fill_ts;
      let fills = rest_client.fetch_user_fills_by_time(&agent, since, None).await?;
      for f in fills {
        manager.apply(WsMessage::UserFill(f.into())).await;
        last_seen_fill_ts = max(last_seen_fill_ts, f.time);
      }
    }
  }
  ```
- [ ] Track `last_seen_fill_ts: u64` as a local variable inside the supervisor (not on `WsStatus` to avoid lock contention).
- [ ] On 429: catch `HlError::RateLimited { wait_ms }` and double `rest_poll_interval` for that connection (cap at 60s); reset on next reconnect.
- [ ] Test: simulate a stale connection (no messages from fake server for `stale_after`); assert that `fetch_user_fills_by_time` was called on the mock REST client.

## Step 7: Wire up `WsSubscriberHandle` to `ServerState`

- [ ] Edit `executor-server/src/state.rs`:
  - Add `pub ws_handle: tokio::sync::Mutex<Option<Arc<WsSubscriberHandle>>>` (Mutex so `Drop` and explicit shutdown can both touch it).
  - Add `pub ws_status: Arc<WsStatus>`.
  - Update `ServerState::new`: accept `ws_status: Arc<WsStatus>`. Default for mock is `WsStatus::disabled()`.
  - Add `Drop` impl that signals shutdown via the handle (best effort; the supervisor's loop will exit).
- [ ] Update mock mode in `main.rs`: pass `Arc::new(WsStatus::disabled())` to `ServerState::new`. No subscriber spawned.
- [ ] Update `integration_rest.rs` test fixture similarly.

## Step 8: `main.rs` real-mode wiring

- [ ] Edit `executor-server/src/main.rs`:
  - After the safety gate is constructed and after `RealHlClient` is upgraded with MetaCache, but before `ServerState::new`:
    ```rust
    let agent_address = signer.address();
    let symbols: Vec<Symbol> = match safety.allow_symbols.as_ref() {
        Some(s) if !s.is_empty() => s.iter().cloned().collect(),
        _ => parse_csv(&args.ws_l2_symbols_fallback),
    };
    let ws_url = match args.base {
        Base::Mainnet => "wss://api.hyperliquid.xyz/ws".to_string(),
        Base::Testnet => "wss://api.hyperliquid-testnet.xyz/ws".to_string(),
    };
    let ws_status = Arc::new(WsStatus::default());
    let ws_cfg = WsSubscriberConfig {
        url: ws_url, agent_address, master_address: master_addr_for_reconcile,
        symbols, stale_after: ..., reconnect_backoff_min: ..., ...
    };
    let manager = Arc::new(WsStateManager::new(state_app_state.clone()));
    let ws_handle = spawn_ws_subscriber(ws_cfg, manager, real_client.clone());
    ```
  - `master_addr_for_reconcile`: re-use the `--master-address` CLI flag we added in PR-C3.
  - Add new flag `--ws-l2-symbols-fallback` (default `"ETH,BTC"`, only used when allow-list is `*`).
  - Pass `ws_status` and (via separate setter or constructor) the handle into `ServerState`.
- [ ] Mock mode: skip `spawn_ws_subscriber`, pass `WsStatus::disabled()`.
- [ ] Run: `cargo run -p executor-server -- --help` and verify the flag shows.

## Step 9: Health endpoint surface

- [ ] Edit `executor-server/src/routes.rs::health`:
  - Read `s.ws_status` fields and merge into the response JSON. Reuse existing `HealthStatus` struct in `executor-core`; the WS fields are already there from PR-3 (`ws_connected`, `ws_message_count`, `ws_reconnect_count`). Update them in `WsStateManager::apply` (already done) and keep `ws_status.connected.load()` in sync at supervisor state changes.
  - Optionally extend `HealthStatus` with `last_user_event` (already there). Done.
- [ ] Test: existing health test still passes; mock mode reports `ws_connected: false` (from disabled WsStatus).

## Step 10: Tests round-up + clippy + fmt + check_ci_local

- [ ] `cargo fmt --all`.
- [ ] `cargo build --workspace`.
- [ ] `cargo test --workspace` — expect previous count + ~16 new (decoder 8, dedup 1, fetch_user_fills 1, ws_subscriber 4, fallback 1, integration 1).
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.
- [ ] `bash scripts/check_ci_local.sh` green.

## Step 11: HANDOFF doc + commit + push + PR

- [ ] Append `## 13. PR-D1 完了 + Phase 4 開始宣言` to `docs/HANDOFF-2026-05-05.md` summarising:
  - what shipped (3 modules, 1 trait flag, 6 SHOULD-FIX taken in)
  - mainnet smoke procedure (use the user's confirmed parameters: `target_size=0.005`, hold long if filled)
  - PR-D2/3/4/5 still queued
- [ ] Branch already created. Split commits:
  1. `docs(spec/plan): PR-D1 ws subscriber + REST fallback`
  2. `feat(executor-hl): WsFill tid + dedup`
  3. `feat(executor-hl): wire::ws frame decoder`
  4. `feat(executor-hl): fetch_user_fills_by_time`
  5. `feat(executor-hl): ws_subscriber supervisor + REST fallback`
  6. `feat(executor-server): wire ws subscriber on real-mode startup`
  7. `feat(executor-server): health endpoint surfaces ws status`
  8. `docs: HANDOFF — PR-D1 完了 + Phase 4 開始宣言`
- [ ] `git push -u origin feat/pr-d1-ws-subscriber`.
- [ ] `gh pr create --base develop ...` (refer to `review_log` PR-D1 entry).

## Step 12: Mainnet smoke (user-driven)

- [ ] (User) `source scripts/load-env.sh && cargo run --release -p executor-server -- --mode real --base mainnet --mainnet-allow-symbols ETH --mainnet-max-notional-usd 25 --master-address 0xfe3e32cd... --ws-l2-symbols-fallback ETH`.
- [ ] Verify startup logs show `ws_subscriber: subscribed userFills+orderUpdates+l2Book(ETH)` and `safety gate / BaselineGuard / executor-server listening`.
- [ ] (User) `curl http://localhost:8085/v1/health | jq` — expect `ws_connected=true`, `ws_message_count > 0` after a few seconds.
- [ ] (User) PASSIVE_FOLLOW order with target_size=0.005 ETH:
  ```bash
  curl -X POST http://localhost:8085/v1/exec \
    -H 'Content-Type: application/json' \
    -H 'X-Operator-ID: me@desk' \
    -d '{
      "algorithm":"passive",
      "symbol":"ETH",
      "intent":"open",
      "target_size":"0.005",
      "params":{"max_total_ms":300000,"repost_poll_ms":2000,"max_book_age_ms":5000}
    }'
  ```
- [ ] Watch `curl /v1/exec/{exec_id}` and `curl /v1/positions` until ETH long appears.
- [ ] User holds the long position (per their instruction: do not unwind after fill).
- [ ] Mark Task #20 done; user says when to proceed to Task #21 ($100 build).

---

## Acceptance gates

- [ ] All Rust tests pass.
- [ ] No clippy warnings.
- [ ] CI green on PR.
- [ ] Mainnet startup shows `ws_connected=true` (user verified).
- [ ] Mainnet smoke: PASSIVE_FOLLOW 0.005 ETH places, fills (or repost cycles indefinitely if market doesn't come down — accept abort after `max_total_ms`).
- [ ] HANDOFF documents the smoke result and any wire-format corrections needed for PR-D2.
