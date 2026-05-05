//! executor-server: axum REST + WS bin (PR-7, PR-C1).
//!
//! Mode selection (clap):
//! - `--mode mock` (default): MockHlClient + MockSigner; runs without keys.
//! - `--mode real`: RealHlClient + Eip712AgentSigner; needs HL_AGENT_PK env.
//!
//! `--base mainnet|testnet` selects the HL endpoint. Only consulted in real mode.
//!
//! Real mode does the bootstrap-then-upgrade dance:
//!   1. Construct an empty-MetaCache RealHlClient.
//!   2. Call MetaCache::build(client, &[None]) — fetches /info meta (default dex).
//!   3. Replace via with_meta() to get a production client.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use executor_core::state::AppState;
use executor_core::types::Address;
use executor_hl::batch_sender::{spawn_batch_sender_with_gate, BatchSenderConfig};
use executor_hl::hl_client::{HlClient, HlConfig, MockHlClient, RealHlClient};
use executor_hl::meta::MetaCache;
use executor_hl::signer::{Eip712AgentSigner, MockSigner, Signer};
use executor_hl::ws_state::WsStateManager;
use executor_hl::ws_subscriber::{spawn_ws_subscriber, WsSubscriberConfig};
use executor_hl::IntentChecker;
use executor_server::{build_app, BaselineGuard, SafetyGate, ServerState};
use rust_decimal::Decimal;
use secrecy::SecretString;

#[derive(Parser, Debug)]
#[command(name = "executor-server", version)]
struct Args {
    /// Backend mode: `mock` for CI/test, `real` for mainnet/testnet.
    #[arg(long, default_value = "mock")]
    mode: Mode,

    /// HL endpoint base. Only relevant in `--mode real`.
    #[arg(long, default_value = "mainnet")]
    base: Base,

    /// Bind address (host:port).
    #[arg(long, env = "EXECUTOR_BIND", default_value = "0.0.0.0:8085")]
    bind: String,

    /// Mainnet allow-list of symbols (comma-separated, e.g. "ETH,BTC").
    /// REQUIRED for `--mode real --base mainnet`.
    /// Use `*` to explicitly allow all (NOT recommended for production).
    #[arg(long, env = "EXECUTOR_MAINNET_ALLOW_SYMBOLS", default_value = "")]
    mainnet_allow_symbols: String,

    /// Per-order USD notional cap. Omit for no cap (NOT recommended for production).
    #[arg(long, env = "EXECUTOR_MAINNET_MAX_NOTIONAL_USD")]
    mainnet_max_notional_usd: Option<u64>,

    /// Enable PR-C3 baseline-diff guard (real mode only). Pass
    /// `--baseline-guard false` (or set `EXECUTOR_BASELINE_GUARD=false`) to
    /// disable. Mock mode always skips the guard regardless of this flag.
    #[arg(
        long,
        env = "EXECUTOR_BASELINE_GUARD",
        default_value_t = true,
        action = clap::ArgAction::Set,
    )]
    baseline_guard: bool,

    /// Master EOA address whose perp positions to monitor (defaults to env
    /// `HL_MASTER_ADDRESS`). Required when `--baseline-guard` is true (real mode).
    #[arg(long, env = "HL_MASTER_ADDRESS")]
    master_address: Option<String>,

    /// Baseline-guard polling interval in seconds.
    #[arg(long, env = "EXECUTOR_BASELINE_POLL_SECS", default_value_t = 60)]
    baseline_poll_secs: u64,

    /// Comma-separated dex list to monitor (e.g. "default,xyz"). `default` and
    /// blank entries are mapped to the default-dex (None).
    #[arg(long, env = "EXECUTOR_BASELINE_DEXES", default_value = "default,xyz")]
    baseline_dexes: String,

    /// szi diff epsilon (decimal). 0 = strict; raise if HL clearinghouseState
    /// shows occasional dust drift unrelated to actual position changes.
    #[arg(long, env = "EXECUTOR_BASELINE_SZI_EPSILON", default_value = "0")]
    baseline_szi_epsilon: String,

    /// Number of consecutive `fetch_account_state` failures before the guard
    /// auto-fires `emergency_stop`.
    #[arg(long, env = "EXECUTOR_BASELINE_MAX_CONSEC_ERRORS", default_value_t = 5)]
    baseline_max_consec_errors: u32,

    /// PR-D1: l2Book symbols to subscribe over WS when `--mainnet-allow-symbols`
    /// is `*` (allow-all). Comma-separated. Without this fallback we wouldn't
    /// know which books to subscribe in the wildcard case.
    #[arg(
        long,
        env = "EXECUTOR_WS_L2_SYMBOLS_FALLBACK",
        default_value = "ETH,BTC"
    )]
    ws_l2_symbols_fallback: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Mock,
    Real,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Base {
    Mainnet,
    Testnet,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,executor_=debug")),
        )
        .with_target(true)
        .compact()
        .init();

    let args = Args::parse();

    let app_state = Arc::new(AppState::new());

    let batch_cfg = BatchSenderConfig {
        flush_interval: Duration::from_millis(100),
        max_batch_size: 50,
    };

    // PR-C2: build the safety gate. In real mode this validates the CLI args
    // (mainnet+empty allow-list is a fatal error per Gemini deep, 2026-05-05).
    let is_mainnet_real = matches!(args.mode, Mode::Real) && matches!(args.base, Base::Mainnet);
    let safety = Arc::new(match args.mode {
        Mode::Mock => SafetyGate::disabled(),
        Mode::Real => SafetyGate::from_args(
            &args.mainnet_allow_symbols,
            args.mainnet_max_notional_usd,
            is_mainnet_real,
        )
        .context("safety gate construction failed")?,
    });
    tracing::info!(
        mode = ?args.mode,
        allow_symbols = ?safety.allow_symbols,
        max_notional_usd = ?safety.max_notional_usd,
        "safety gate constructed",
    );

    // `spawn_batch_sender_with_gate<C>` requires a concrete sized `C: HlClient`,
    // so we build the BatchSender inside each match arm where the type is known
    // and erase to `Arc<dyn HlClient>` only for `ServerState::new`.
    let (hl_client, signer, batch_sender, batch_handle): (
        Arc<dyn HlClient>,
        Arc<dyn Signer>,
        _,
        _,
    ) = match args.mode {
        Mode::Mock => {
            tracing::info!("starting in mock mode");
            let mock_hl = Arc::new(MockHlClient::new());
            let mock_signer: Arc<dyn Signer> = Arc::new(MockSigner::new());
            let (batch_sender, batch_handle) =
                spawn_batch_sender_with_gate(mock_hl.clone(), None, batch_cfg);
            let hl_dyn: Arc<dyn HlClient> = mock_hl;
            (hl_dyn, mock_signer, batch_sender, batch_handle)
        }
        Mode::Real => {
            let is_mainnet = matches!(args.base, Base::Mainnet);
            tracing::info!(?args.base, "starting in real mode");
            let config = match args.base {
                Base::Mainnet => HlConfig::mainnet(),
                Base::Testnet => HlConfig::testnet(),
            };
            let pk = std::env::var("HL_AGENT_PK")
                .context("HL_AGENT_PK env required for --mode real (run scripts/load-env.sh)")?;
            let signer: Arc<dyn Signer> = Arc::new(
                Eip712AgentSigner::from_secret(SecretString::new(pk.into()), is_mainnet)
                    .context("Eip712AgentSigner::from_secret failed")?,
            );

            // PR-D1+ hotfix: verify the PK actually corresponds to the agent
            // address declared in the environment. If they disagree, every
            // place_orders call will hit HL with a signature ecrecover'd to
            // an address HL doesn't know about ("User or API Wallet ... does
            // not exist"). EIP-712 signatures don't error on mismatch; they
            // just resolve to an arbitrary address. Catch this at startup.
            let pk_derived = signer.address();
            tracing::info!(agent_pk_derived = %pk_derived, "agent address derived from HL_AGENT_PK");
            match std::env::var("HL_AGENT_ADDRESS") {
                Ok(declared) => {
                    let declared_norm = declared.trim().to_ascii_lowercase();
                    let derived_norm = pk_derived.as_str().to_ascii_lowercase();
                    if declared_norm != derived_norm {
                        anyhow::bail!(
                            "agent address mismatch: HL_AGENT_PK derives `{derived_norm}` \
                             but HL_AGENT_ADDRESS env says `{declared_norm}`. \
                             The signature would ecrecover to a wallet HL doesn't recognize, \
                             producing 'User or API Wallet does not exist' errors. \
                             Either rotate HL_AGENT_PK to the correct key or update \
                             HL_AGENT_ADDRESS / .env.develop to the address derived from the key. \
                             Tip: HL UI > API page shows authorized agent wallets for the master."
                        );
                    }
                    tracing::info!(
                        agent = %declared_norm,
                        "agent address mismatch check passed (PK ↔ HL_AGENT_ADDRESS)"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        agent_pk_derived = %pk_derived,
                        "HL_AGENT_ADDRESS env not set — skipping PK↔address consistency check. \
                         Set HL_AGENT_ADDRESS in .env.develop so startup can fail-fast on \
                         key/address drift."
                    );
                }
            }

            let bootstrap = RealHlClient::bootstrap(config, signer.clone());
            let meta = Arc::new(
                MetaCache::build(&bootstrap, &[None])
                    .await
                    .context("MetaCache::build (default dex) failed at startup")?,
            );
            tracing::info!(symbols = meta.len(), "MetaCache built (default dex)");

            // PR-D1+ hotfix: probe HL `userRole` to confirm the agent is
            // registered. HL refuses signed actions from unregistered keys
            // with "top_level_err: User or API Wallet ... does not exist."
            // Catching that at startup beats waiting for the first order to
            // explode at runtime.
            match bootstrap.fetch_user_role(&pk_derived).await {
                Ok(role) => {
                    use executor_hl::Role;
                    match &role {
                        Role::Agent { master } => {
                            tracing::info!(
                                agent = %pk_derived,
                                role_master = %master,
                                "HL userRole probe: agent registered, ready"
                            );
                            // If the operator passed --master-address, sanity-check it.
                            if let Some(declared_master) = args.master_address.as_deref() {
                                let declared = declared_master.trim().to_ascii_lowercase();
                                let role_master = master.as_str().to_ascii_lowercase();
                                if declared != role_master {
                                    anyhow::bail!(
                                        "master address mismatch: --master-address = `{declared}` \
                                         but HL says agent `{}` belongs to master `{role_master}`. \
                                         BaselineGuard would monitor the wrong account.",
                                        pk_derived
                                    );
                                }
                            }
                        }
                        other => {
                            tracing::error!(
                                agent = %pk_derived,
                                ?other,
                                "HL userRole probe: agent is NOT registered as an Agent. \
                                 Place_orders will fail with 'User or API Wallet does not exist'."
                            );
                            anyhow::bail!(
                                "agent {} is not registered with HL as an Agent (role={:?}). \
                                 Authorize this address as an API wallet on the master \
                                 account first, or rotate HL_AGENT_PK to a registered agent.",
                                pk_derived,
                                other
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        agent = %pk_derived,
                        error = %e,
                        "HL userRole probe failed (network/info issue); proceeding without registration check"
                    );
                }
            }

            let real_client = Arc::new(bootstrap.with_meta(meta));
            let gate_dyn: Arc<dyn IntentChecker> = safety.clone();
            let (batch_sender, batch_handle) =
                spawn_batch_sender_with_gate(real_client.clone(), Some(gate_dyn), batch_cfg);
            let hl_dyn: Arc<dyn HlClient> = real_client;
            (hl_dyn, signer, batch_sender, batch_handle)
        }
    };

    let mut state_owned = ServerState::new(
        app_state.clone(),
        hl_client,
        signer.clone(),
        batch_sender,
        batch_handle,
        safety.clone(),
    );

    // PR-D1: spawn WS subscriber in real mode. Mock mode keeps the disabled
    // ws_status default — health endpoint reports `ws_connected: false`.
    if matches!(args.mode, Mode::Real) {
        let agent_addr = signer.address();
        let master_addr_str = args
            .master_address
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!(
                "--master-address (or HL_MASTER_ADDRESS env) required for --mode real (used by WS reconcile)"
            ))?;
        let master_addr = Address::new(master_addr_str);
        let symbols: Vec<executor_core::symbol::Symbol> = match safety.allow_symbols.as_ref() {
            Some(allow) if !allow.is_empty() => allow.iter().cloned().collect(),
            _ => args
                .ws_l2_symbols_fallback
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(executor_core::symbol::Symbol::new)
                .collect(),
        };
        let ws_url = match args.base {
            Base::Mainnet => "wss://api.hyperliquid.xyz/ws".to_string(),
            Base::Testnet => "wss://api.hyperliquid-testnet.xyz/ws".to_string(),
        };
        tracing::info!(
            agent = %agent_addr,
            master = %master_addr,
            symbols = ?symbols,
            url = %ws_url,
            "ws_subscriber: spawning",
        );
        let ws_cfg = WsSubscriberConfig {
            url: ws_url,
            agent_address: agent_addr,
            master_address: master_addr,
            symbols,
            ..Default::default()
        };
        let manager = Arc::new(WsStateManager::new(app_state.clone()));
        let ws_handle = spawn_ws_subscriber(ws_cfg, manager, state_owned.hl_client.clone());
        state_owned.install_ws_subscriber(ws_handle);
    }

    let state = Arc::new(state_owned);

    // PR-C3: capture baseline + spawn tick task. Real mode only.
    if matches!(args.mode, Mode::Real) && args.baseline_guard {
        let master = args
            .master_address
            .as_deref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--master-address (or HL_MASTER_ADDRESS env) required for --mode real with baseline-guard. \
                     Pass --baseline-guard=false to disable."
                )
            })?;
        let dexes = parse_dexes_csv(&args.baseline_dexes);
        let szi_epsilon: Decimal = args.baseline_szi_epsilon.parse().with_context(|| {
            format!(
                "invalid --baseline-szi-epsilon: {}",
                args.baseline_szi_epsilon
            )
        })?;
        // PR-D6: hand the safety-gate allow-list to BaselineGuard so symbols
        // the algorithm is explicitly authorised to trade (e.g. ETH) are
        // exempt from baseline drift checks. Symbols outside the allow-list
        // (HYPE / TON / xyz:META / ...) remain strictly guarded — operator
        // hasn't authorised the bot to touch them. Wildcard `*` resolves to
        // `allow_symbols=None` in SafetyGate; in that mode we keep the old
        // "guard everything" behavior, since the operator hasn't named a
        // specific symbol they want exempt.
        let excluded_symbols: std::collections::HashSet<executor_core::symbol::Symbol> =
            state.safety.allow_symbols.clone().unwrap_or_default();
        let guard = BaselineGuard::capture(
            state.hl_client.as_ref(),
            Address::new(master),
            dexes,
            Duration::from_secs(args.baseline_poll_secs),
            szi_epsilon,
            excluded_symbols,
        )
        .await
        .context("BaselineGuard::capture failed")?;
        tracing::info!(
            master = master,
            dexes = ?guard.dexes,
            baseline_size = guard.baseline.len(),
            excluded_size = guard.excluded_symbols.len(),
            poll_secs = guard.poll_interval.as_secs(),
            szi_epsilon = ?guard.szi_epsilon,
            "BaselineGuard captured",
        );

        let guard = Arc::new(guard);
        let task_state = state.clone();
        let task_client = state.hl_client.clone();
        let max_consec = args.baseline_max_consec_errors;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(guard.poll_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Skip the immediate first tick — `interval(d)` fires once at t=0.
            ticker.tick().await;
            let mut consec_errors: u32 = 0;
            loop {
                ticker.tick().await;
                match guard.check_once(task_client.as_ref()).await {
                    Ok(violations) if violations.is_empty() => {
                        consec_errors = 0;
                        tracing::trace!("baseline_guard: tick clean");
                    }
                    Ok(violations) => {
                        tracing::error!(?violations, "BASELINE VIOLATION DETECTED");
                        let _ = executor_server::routes::execute_emergency_stop(
                            &task_state,
                            "baseline_guard",
                        )
                        .await;
                        tracing::error!("baseline_guard: emergency_stop fired, exiting tick loop");
                        break;
                    }
                    Err(e) => {
                        consec_errors += 1;
                        tracing::warn!(
                            error = %e,
                            consec_errors,
                            max_consec,
                            "baseline_guard: fetch failed",
                        );
                        if consec_errors >= max_consec {
                            tracing::error!(
                                consec_errors,
                                "baseline_guard: too many consecutive failures, firing emergency_stop"
                            );
                            let _ = executor_server::routes::execute_emergency_stop(
                                &task_state,
                                "baseline_guard_consec_errors",
                            )
                            .await;
                            break;
                        }
                    }
                }
            }
        });
    }

    let app = build_app(state);
    tracing::info!(bind = %args.bind, "executor-server listening");
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

/// Parse `"default,xyz"` → `[None, Some("xyz")]`. Empty entries and the
/// literal `default` (case-insensitive) are mapped to the default-dex.
fn parse_dexes_csv(s: &str) -> Vec<Option<String>> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.eq_ignore_ascii_case("default") {
                None
            } else {
                Some(s.to_string())
            }
        })
        .collect()
}
