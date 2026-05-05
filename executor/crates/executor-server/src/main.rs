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
use executor_hl::batch_sender::{spawn_batch_sender, BatchSenderConfig};
use executor_hl::hl_client::{HlClient, HlConfig, MockHlClient, RealHlClient};
use executor_hl::meta::MetaCache;
use executor_hl::signer::{Eip712AgentSigner, MockSigner, Signer};
use executor_server::{build_app, ServerState};
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

    // `spawn_batch_sender<C>` requires a concrete sized `C: HlClient`, so we
    // build the BatchSender inside each match arm where the type is known and
    // erase to `Arc<dyn HlClient>` only for `ServerState::new`.
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
            let (batch_sender, batch_handle) = spawn_batch_sender(mock_hl.clone(), batch_cfg);
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
            let bootstrap = RealHlClient::bootstrap(config, signer.clone());
            let meta = Arc::new(
                MetaCache::build(&bootstrap, &[None])
                    .await
                    .context("MetaCache::build (default dex) failed at startup")?,
            );
            tracing::info!(symbols = meta.len(), "MetaCache built (default dex)");
            let real_client = Arc::new(bootstrap.with_meta(meta));
            let (batch_sender, batch_handle) = spawn_batch_sender(real_client.clone(), batch_cfg);
            let hl_dyn: Arc<dyn HlClient> = real_client;
            (hl_dyn, signer, batch_sender, batch_handle)
        }
    };

    let state = Arc::new(ServerState::new(
        app_state,
        hl_client,
        signer,
        batch_sender,
        batch_handle,
    ));

    let app = build_app(state);
    tracing::info!(bind = %args.bind, "executor-server listening");
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}
