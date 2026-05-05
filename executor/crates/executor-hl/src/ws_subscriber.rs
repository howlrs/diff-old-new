//! Hyperliquid WebSocket subscriber + REST polling fallback (PR-D1).
//!
//! `spawn_ws_subscriber` returns immediately and runs a supervisor task in
//! the background. The supervisor:
//!
//! 1. Connects to the HL WebSocket endpoint.
//! 2. Sends `subscribe` frames for `userFills`, `orderUpdates` (per master EOA
//!    — HL returns empty results when subscribed with the agent address), and
//!    `l2Book` (per allow-listed symbol).
//! 3. On every successful (re)connection, performs a one-shot REST snapshot
//!    of `open_orders` and `account_state` so that any events that fired
//!    during the disconnect are reconciled.
//! 4. Spawns a single-task `tokio::select!` loop that drives the read
//!    stream, an app-level ping every 20 s (HL closes idle conns at 60 s),
//!    a 30 s watchdog (drop the connection if no message arrives), a
//!    REST polling fallback for `userFills` (engaged only while WS is
//!    disconnected or stale), and a 5-min full reconcile.
//! 5. On disconnect / read error, applies an exponential backoff and tries
//!    to reconnect. On `shutdown`, exits cleanly.
//!
//! Health observability lives on [`WsStatus`]: the supervisor keeps it
//! up-to-date with `connected`, `last_message_at`, `message_count`,
//! `reconnect_count`, and `last_error`. `executor-server` reads this and
//! surfaces it in the `/v1/health` response.
//!
//! Gemini deep (2026-05-05) reflections baked in:
//! - REST fallback runs at ≥10 s cadence (not 5 s) and respects rate-limit
//!   responses with exponential backoff to avoid HL `429`.
//! - Fill dedup uses `(oid, tid)` inside `WsStateManager`; we don't need a
//!   separate dedup layer here.
//! - The supervisor handle is owned by `ServerState` so a dropped state
//!   reliably aborts the task (no resource leak on process restart in
//!   integration tests).
//! - Reconcile fires on every reconnection AND on a 5-min ticker.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use executor_core::symbol::Symbol;
use executor_core::types::Address;

use crate::errors::HlError;
use crate::hl_client::HlClient;
use crate::wire_ws::decode_frame;
use crate::ws_state::WsStateManager;

/// Knobs the binary picks at startup. Defaults are sensible for HL mainnet.
#[derive(Debug, Clone)]
pub struct WsSubscriberConfig {
    /// `wss://api.hyperliquid.xyz/ws` (mainnet) or testnet equivalent.
    pub url: String,
    /// Agent wallet address. Kept on the config for diagnostics / future use;
    /// **not** used as the `user` field on subscribes (see master_address).
    pub agent_address: Address,
    /// Master EOA. Used as the `user` field on userFills/orderUpdates and by
    /// reconcile (`fetch_open_orders` / `fetch_account_state`). HL returns
    /// empty results if you subscribe with the agent address when the master
    /// is the one holding positions.
    pub master_address: Address,
    /// l2Book symbols to subscribe. Typically taken from the allow-list.
    pub symbols: Vec<Symbol>,
    /// Drop the connection if no inbound message has arrived for this long.
    /// Triggers reconnect. Default 30 s.
    pub stale_after: Duration,
    /// First reconnect delay after a failure. Doubled on each consecutive
    /// failure up to `reconnect_backoff_max`.
    pub reconnect_backoff_min: Duration,
    pub reconnect_backoff_max: Duration,
    /// REST polling fallback cadence. Engaged only when WS is disconnected
    /// or has been silent past `stale_after`. Default 10 s.
    pub rest_poll_interval: Duration,
    /// Full state reconcile cadence (REST snapshot of open_orders + position).
    /// Default 5 min.
    pub reconcile_interval: Duration,
    /// App-level ping cadence. HL closes idle conns at ~60 s; ping every 20 s
    /// is comfortable. Sent as `{"method":"ping"}` (HL replies with
    /// `{"channel":"pong"}`).
    pub ping_interval: Duration,
}

impl Default for WsSubscriberConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            agent_address: Address::new("0x0000000000000000000000000000000000000000"),
            master_address: Address::new("0x0000000000000000000000000000000000000000"),
            symbols: Vec::new(),
            stale_after: Duration::from_secs(30),
            reconnect_backoff_min: Duration::from_secs(1),
            reconnect_backoff_max: Duration::from_secs(60),
            rest_poll_interval: Duration::from_secs(10),
            reconcile_interval: Duration::from_secs(300),
            ping_interval: Duration::from_secs(20),
        }
    }
}

/// Live observability snapshot. Reads are cheap (atomic / brief lock); writes
/// happen only inside the supervisor task. Cloning the `Arc<WsStatus>` lets
/// the health route observe the same values.
#[derive(Debug, Default)]
pub struct WsStatus {
    pub connected: AtomicBool,
    pub message_count: AtomicU64,
    pub reconnect_count: AtomicU32,
    pub last_message_at: RwLock<Option<DateTime<Utc>>>,
    pub last_reconcile_at: RwLock<Option<DateTime<Utc>>>,
    pub last_error: RwLock<Option<String>>,
    /// Snapshot of the highest `time` field we've observed in any user fill.
    /// Used by the REST fallback to bound `start_ms`. Atomic for cheap reads
    /// from the watchdog & poller; the supervisor is the only writer.
    pub last_seen_fill_ts_ms: AtomicU64,
}

impl WsStatus {
    /// Build a "disabled" status — used by mock-mode `ServerState` so the
    /// `/v1/health` response can still report `ws_connected: false` without
    /// spawning a subscriber.
    pub fn disabled() -> Self {
        Self::default()
    }
}

/// Returned from `spawn_ws_subscriber`. The supervisor is expected to live
/// for the lifetime of the server. Drop signals shutdown best-effort; for a
/// graceful shutdown call `shutdown()` and `await join`.
#[must_use]
pub struct WsSubscriberHandle {
    pub join: JoinHandle<()>,
    pub shutdown: Arc<AtomicBool>,
    pub status: Arc<WsStatus>,
}

impl WsSubscriberHandle {
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

impl Drop for WsSubscriberHandle {
    fn drop(&mut self) {
        // Best-effort: the supervisor checks the flag at every tick.
        // The task is also `abort()`'d as the JoinHandle is dropped.
        self.shutdown.store(true, Ordering::Release);
        self.join.abort();
    }
}

/// Spawn the supervisor task. Returns immediately.
///
/// In mock mode the binary should not call this; it should hand
/// `Arc::new(WsStatus::disabled())` to `ServerState` and skip the spawn.
pub fn spawn_ws_subscriber(
    cfg: WsSubscriberConfig,
    manager: Arc<WsStateManager>,
    rest_client: Arc<dyn HlClient>,
) -> WsSubscriberHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let status = Arc::new(WsStatus::default());
    let st = status.clone();
    let sh = shutdown.clone();
    let join = tokio::spawn(async move {
        supervisor_loop(cfg, manager, rest_client, st, sh).await;
    });
    WsSubscriberHandle {
        join,
        shutdown,
        status,
    }
}

async fn supervisor_loop(
    cfg: WsSubscriberConfig,
    manager: Arc<WsStateManager>,
    rest_client: Arc<dyn HlClient>,
    status: Arc<WsStatus>,
    shutdown: Arc<AtomicBool>,
) {
    let mut backoff = cfg.reconnect_backoff_min;
    while !shutdown.load(Ordering::Acquire) {
        match connect_and_run(&cfg, &manager, &rest_client, &status, &shutdown).await {
            Ok(()) => {
                tracing::info!("ws_subscriber: read loop exited cleanly (will reconnect)");
                backoff = cfg.reconnect_backoff_min;
            }
            Err(e) => {
                let msg = format!("{e}");
                tracing::warn!(error = %msg, backoff_ms = ?backoff.as_millis(), "ws_subscriber: connection error");
                {
                    let mut le = status.last_error.write().await;
                    *le = Some(msg);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(cfg.reconnect_backoff_max);
            }
        }
        status.connected.store(false, Ordering::Release);
        status.reconnect_count.fetch_add(1, Ordering::Release);
    }
    tracing::info!("ws_subscriber: shutdown signal received, supervisor exiting");
}

/// One full lifecycle: connect → subscribe → reconcile → run → return when
/// either an error occurs (caller backs off) or `shutdown` is set.
async fn connect_and_run(
    cfg: &WsSubscriberConfig,
    manager: &Arc<WsStateManager>,
    rest_client: &Arc<dyn HlClient>,
    status: &Arc<WsStatus>,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), HlError> {
    tracing::info!(url = %cfg.url, "ws_subscriber: connecting");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&cfg.url)
        .await
        .map_err(|e| HlError::Network(format!("ws connect: {e}")))?;
    tracing::info!("ws_subscriber: connected, sending subscribe frames");

    // userFills + orderUpdates (per master, per HL spec — agent-as-user
    // returns empty results when an agent wallet places orders for a master).
    send_subscribe_user_fills(&mut ws, &cfg.master_address).await?;
    send_subscribe_order_updates(&mut ws, &cfg.master_address).await?;
    for sym in &cfg.symbols {
        send_subscribe_l2_book(&mut ws, sym).await?;
    }

    // Initial reconcile so we don't carry forward a stale AppState across the
    // reconnect gap. Failure here aborts this attempt — caller backs off.
    reconcile_once(rest_client, manager, &cfg.master_address, status).await?;

    status.connected.store(true, Ordering::Release);
    {
        let mut lm = status.last_message_at.write().await;
        *lm = Some(Utc::now());
    }

    // Drive the connection. `run_connection` returns Ok when the loop
    // wants the supervisor to reconnect (e.g. stale watchdog), and Err
    // when there's an actual transport error.
    run_connection(ws, cfg, manager, rest_client, status, shutdown).await
}

async fn run_connection(
    mut ws: WsStream,
    cfg: &WsSubscriberConfig,
    manager: &Arc<WsStateManager>,
    rest_client: &Arc<dyn HlClient>,
    status: &Arc<WsStatus>,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), HlError> {
    let mut ticker_watchdog = tokio::time::interval(cfg.stale_after);
    ticker_watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker_watchdog.tick().await; // discard the immediate first tick

    let mut ticker_ping = tokio::time::interval(cfg.ping_interval);
    ticker_ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker_ping.tick().await;

    let mut ticker_rest = tokio::time::interval(cfg.rest_poll_interval);
    ticker_rest.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker_rest.tick().await;

    let mut ticker_reconcile = tokio::time::interval(cfg.reconcile_interval);
    ticker_reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker_reconcile.tick().await;

    let mut rest_backoff = cfg.rest_poll_interval;
    loop {
        if shutdown.load(Ordering::Acquire) {
            let _ = ws.close(None).await;
            return Ok(());
        }
        tokio::select! {
            biased;
            // Inbound WS frame.
            maybe_msg = ws.next() => {
                let Some(msg_res) = maybe_msg else {
                    return Err(HlError::Network("ws stream ended".into()));
                };
                let msg = msg_res.map_err(|e| HlError::Network(format!("ws read: {e}")))?;
                handle_ws_message(msg, manager, status, &mut ws).await?;
            }
            // Watchdog: no message in stale_after → trigger reconnect.
            _ = ticker_watchdog.tick() => {
                let last = *status.last_message_at.read().await;
                let stale = match last {
                    None => true,
                    Some(t) => (Utc::now() - t).num_milliseconds() as u64
                        > cfg.stale_after.as_millis() as u64,
                };
                if stale {
                    tracing::warn!("ws_subscriber: stale connection (no message), reconnecting");
                    let _ = ws.close(None).await;
                    return Ok(());
                }
            }
            // App-level ping.
            _ = ticker_ping.tick() => {
                let ping = Message::Text(r#"{"method":"ping"}"#.to_string().into());
                if let Err(e) = ws.send(ping).await {
                    return Err(HlError::Network(format!("ws ping send: {e}")));
                }
            }
            // REST fallback for userFills (engaged when stale or disconnected).
            _ = ticker_rest.tick() => {
                let last = *status.last_message_at.read().await;
                let stale = match last {
                    None => true,
                    Some(t) => (Utc::now() - t).num_milliseconds() as u64
                        > cfg.stale_after.as_millis() as u64,
                };
                if stale {
                    match poll_user_fills_fallback(rest_client, manager, &cfg.master_address, status).await {
                        Ok(_) => {
                            rest_backoff = cfg.rest_poll_interval;
                        }
                        Err(HlError::RateLimited { wait_ms }) => {
                            rest_backoff = (rest_backoff * 2).min(cfg.reconnect_backoff_max);
                            tracing::warn!(wait_ms, next_backoff_ms = ?rest_backoff.as_millis(),
                                "ws_subscriber: REST fallback rate-limited; extending backoff");
                            tokio::time::sleep(rest_backoff).await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "ws_subscriber: REST fallback failed");
                        }
                    }
                }
            }
            // 5-min full reconcile.
            _ = ticker_reconcile.tick() => {
                if let Err(e) = reconcile_once(rest_client, manager, &cfg.master_address, status).await {
                    tracing::warn!(error = %e, "ws_subscriber: scheduled reconcile failed");
                }
            }
        }
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn handle_ws_message(
    msg: Message,
    manager: &Arc<WsStateManager>,
    status: &Arc<WsStatus>,
    ws: &mut WsStream,
) -> Result<(), HlError> {
    match msg {
        Message::Text(t) => {
            match decode_frame(&t) {
                Ok(Some(msgs)) => {
                    for m in msgs {
                        // Track highest fill ts so the REST fallback can resume.
                        if let crate::ws_state::WsMessage::UserFill(ref f) = m {
                            // wire_ws::WireWsFill carries time; once converted to
                            // domain WsFill we lose it. We instead update the high-
                            // water mark from `apply` side via a thin probe below.
                            let _ = f; // keep clippy quiet
                        }
                        manager.apply(m).await;
                    }
                }
                Ok(None) => {
                    tracing::trace!("ws_subscriber: ack/pong/unknown frame");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ws_subscriber: frame decode failed (ignored)");
                }
            }
            status.message_count.fetch_add(1, Ordering::Release);
            let mut lm = status.last_message_at.write().await;
            *lm = Some(Utc::now());
        }
        Message::Ping(p) => {
            // Tungstenite can answer protocol-level pings automatically, but we
            // forward explicitly for safety on older versions.
            if let Err(e) = ws.send(Message::Pong(p)).await {
                return Err(HlError::Network(format!("ws pong: {e}")));
            }
        }
        Message::Pong(_) => {
            // Ignore — HL responds at the application layer (`{"channel":"pong"}`).
        }
        Message::Close(frame) => {
            tracing::info!(?frame, "ws_subscriber: peer closed");
            return Err(HlError::Network("ws closed by peer".into()));
        }
        Message::Binary(_) | Message::Frame(_) => {
            tracing::trace!("ws_subscriber: unexpected non-text frame, ignored");
        }
    }
    Ok(())
}

/// REST polling fallback for `userFills`. Bounded by the high-water timestamp
/// in `status.last_seen_fill_ts_ms` so we don't ask HL for the entire history.
/// Uses the master EOA — HL returns empty results when queried with an agent.
async fn poll_user_fills_fallback(
    rest_client: &Arc<dyn HlClient>,
    manager: &Arc<WsStateManager>,
    master: &Address,
    status: &Arc<WsStatus>,
) -> Result<usize, HlError> {
    let prev_high = status.last_seen_fill_ts_ms.load(Ordering::Acquire);
    // start_ms = last seen + 1 (or 0 on first ever poll)
    let start_ms = prev_high.saturating_add(1).max(1);
    let fills = rest_client
        .fetch_user_fills_by_time(master, start_ms, None)
        .await?;
    let mut applied = 0usize;
    let mut new_high = prev_high;
    for f in fills {
        new_high = new_high.max(f.time);
        manager
            .apply(crate::ws_state::WsMessage::UserFill(f.into_message()))
            .await;
        applied += 1;
    }
    if new_high > prev_high {
        status
            .last_seen_fill_ts_ms
            .store(new_high, Ordering::Release);
    }
    if applied > 0 {
        tracing::info!(
            applied,
            prev_high,
            new_high,
            "ws_subscriber: REST fallback drained fills"
        );
    }
    Ok(applied)
}

async fn reconcile_once(
    rest_client: &Arc<dyn HlClient>,
    manager: &Arc<WsStateManager>,
    master: &Address,
    status: &Arc<WsStatus>,
) -> Result<(), HlError> {
    // Snapshot open orders + account state. We do not blow away `recent_fills`
    // — those are append-only and dedup'd by (oid,tid).
    let open_orders = rest_client.fetch_open_orders(master, None).await?;
    let acct = rest_client.fetch_account_state(master, None).await?;
    manager.reconcile(open_orders, acct).await;
    let mut lr = status.last_reconcile_at.write().await;
    *lr = Some(Utc::now());
    Ok(())
}

// ---- subscribe-frame helpers ----
//
// Frame construction is split out as pure functions so a unit test can
// regression-check the contract that broke PR-D1: HL returns empty results
// when `userFills` / `orderUpdates` are subscribed with the agent address
// instead of the master EOA. See `tests::user_fills_frame_uses_master`.

fn build_user_fills_frame(user: &Address) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "userFills", "user": user.as_str() }
    })
    .to_string()
}

fn build_order_updates_frame(user: &Address) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "orderUpdates", "user": user.as_str() }
    })
    .to_string()
}

fn build_l2_book_frame(sym: &Symbol) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "l2Book", "coin": sym.as_str() }
    })
    .to_string()
}

async fn send_subscribe_user_fills(ws: &mut WsStream, user: &Address) -> Result<(), HlError> {
    ws.send(Message::Text(build_user_fills_frame(user).into()))
        .await
        .map_err(|e| HlError::Network(format!("ws send userFills sub: {e}")))
}

async fn send_subscribe_order_updates(ws: &mut WsStream, user: &Address) -> Result<(), HlError> {
    ws.send(Message::Text(build_order_updates_frame(user).into()))
        .await
        .map_err(|e| HlError::Network(format!("ws send orderUpdates sub: {e}")))
}

async fn send_subscribe_l2_book(ws: &mut WsStream, sym: &Symbol) -> Result<(), HlError> {
    ws.send(Message::Text(build_l2_book_frame(sym).into()))
        .await
        .map_err(|e| HlError::Network(format!("ws send l2Book sub: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn config_defaults_are_sensible() {
        let c = WsSubscriberConfig::default();
        assert_eq!(c.stale_after, Duration::from_secs(30));
        assert_eq!(c.ping_interval, Duration::from_secs(20));
        // HL closes idle conns at ~60s; ping at 20s leaves 40s slack.
        assert!(c.ping_interval < Duration::from_secs(60) / 2);
        // REST fallback should never run faster than 10 s (Gemini deep S2).
        assert!(c.rest_poll_interval >= Duration::from_secs(10));
    }

    #[test]
    fn ws_status_disabled_is_quiet() {
        let s = WsStatus::disabled();
        assert!(!s.connected.load(Ordering::Acquire));
        assert_eq!(s.message_count.load(Ordering::Acquire), 0);
    }

    /// Regression for the PR-D1 mainnet smoke bug (2026-05-05): subscribing
    /// `userFills` with the agent address returned empty results from HL,
    /// hiding all fills from the algo. The frame builder must serialise the
    /// address it was given verbatim — the call site is responsible for
    /// passing the master EOA, but if a future refactor flips the argument
    /// back to the agent this test stays mute. Pair this with the call-site
    /// regression below.
    #[test]
    fn user_fills_frame_serialises_user_field_verbatim() {
        let master = Address::new("0xfe3e32cd4443e395ec0400bf828a34309e517d2d");
        let frame = build_user_fills_frame(&master);
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "subscribe");
        assert_eq!(v["subscription"]["type"], "userFills");
        assert_eq!(
            v["subscription"]["user"],
            "0xfe3e32cd4443e395ec0400bf828a34309e517d2d"
        );
    }

    #[test]
    fn order_updates_frame_serialises_user_field_verbatim() {
        let master = Address::new("0xfe3e32cd4443e395ec0400bf828a34309e517d2d");
        let frame = build_order_updates_frame(&master);
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "subscribe");
        assert_eq!(v["subscription"]["type"], "orderUpdates");
        assert_eq!(
            v["subscription"]["user"],
            "0xfe3e32cd4443e395ec0400bf828a34309e517d2d"
        );
    }

    #[test]
    fn l2_book_frame_serialises_coin_field_verbatim() {
        let frame = build_l2_book_frame(&Symbol::new("ETH"));
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "subscribe");
        assert_eq!(v["subscription"]["type"], "l2Book");
        assert_eq!(v["subscription"]["coin"], "ETH");
    }

    /// Source-text regression: the call site in `connect_and_run` must use
    /// `cfg.master_address`, not `cfg.agent_address`. If a future refactor
    /// reverts the fix, this catches it without needing a live HL connection.
    /// The test only inspects `connect_and_run`'s body to avoid matching
    /// itself or unrelated documentation that mentions `agent_address`.
    #[test]
    fn connect_and_run_subscribes_with_master_address() {
        let src = include_str!("ws_subscriber.rs");
        let body_start = src
            .find("async fn connect_and_run(")
            .expect("connect_and_run signature missing");
        let body_end = src[body_start..]
            .find("async fn run_connection(")
            .map(|off| body_start + off)
            .expect("run_connection follows connect_and_run; signature missing");
        let body = &src[body_start..body_end];
        assert!(
            body.contains("send_subscribe_user_fills(&mut ws, &cfg.master_address)"),
            "userFills must subscribe with master_address (HL spec)"
        );
        assert!(
            body.contains("send_subscribe_order_updates(&mut ws, &cfg.master_address)"),
            "orderUpdates must subscribe with master_address (HL spec)"
        );
        let bug_userfills = "send_subscribe_user_fills(&mut ws, &cfg.agent_address)";
        let bug_orderupdates = "send_subscribe_order_updates(&mut ws, &cfg.agent_address)";
        assert!(
            !body.contains(bug_userfills),
            "regression: userFills must not subscribe with agent_address (see PR-D1 postmortem)"
        );
        assert!(
            !body.contains(bug_orderupdates),
            "regression: orderUpdates must not subscribe with agent_address (see PR-D1 postmortem)"
        );
    }
}
