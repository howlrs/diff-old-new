//! executor-cli: dry-run CLI client for the executor-server REST API.
//!
//! Usage examples:
//!
//! ```bash
//! executor-cli health
//! executor-cli book BTC
//! executor-cli exec --algo market --symbol BTC --intent open --size 0.1
//! executor-cli status <exec_id>
//! executor-cli cancel <exec_id>
//! executor-cli emergency-stop
//! ```

#![forbid(unsafe_code)]

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(
    name = "executor-cli",
    version,
    about = "Dry-run CLI for the Hyperliquid executor server"
)]
struct Cli {
    /// Server base URL (no trailing slash).
    #[arg(long, env = "EXECUTOR_URL", default_value = "http://127.0.0.1:8085")]
    url: String,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// GET /v1/health
    Health,
    /// GET /v1/positions
    Positions,
    /// GET /v1/book/<symbol>
    Book { symbol: String },
    /// POST /v1/exec
    Exec {
        #[arg(long)]
        algo: String,
        #[arg(long)]
        symbol: String,
        #[arg(long, default_value = "open")]
        intent: String,
        #[arg(long)]
        size: String,
        /// JSON object of algorithm params, e.g. '{"slice_count":5}'.
        #[arg(long, default_value = "{}")]
        params: String,
    },
    /// GET /v1/exec/<id>
    Status { id: String },
    /// POST /v1/exec/<id>/cancel
    Cancel { id: String },
    /// POST /v1/emergency_stop
    EmergencyStop,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let body = match cli.command {
        Cmd::Health => get(&client, &format!("{}/v1/health", cli.url)).await?,
        Cmd::Positions => get(&client, &format!("{}/v1/positions", cli.url)).await?,
        Cmd::Book { symbol } => get(&client, &format!("{}/v1/book/{symbol}", cli.url)).await?,
        Cmd::Status { id } => get(&client, &format!("{}/v1/exec/{id}", cli.url)).await?,
        Cmd::Cancel { id } => {
            post(
                &client,
                &format!("{}/v1/exec/{id}/cancel", cli.url),
                json!({}),
            )
            .await?
        }
        Cmd::EmergencyStop => {
            post(
                &client,
                &format!("{}/v1/emergency_stop", cli.url),
                json!({}),
            )
            .await?
        }
        Cmd::Exec {
            algo,
            symbol,
            intent,
            size,
            params,
        } => {
            let params_json: serde_json::Value = serde_json::from_str(&params)?;
            let req = json!({
                "algorithm": algo,
                "symbol": symbol,
                "intent": intent,
                "target_size": size,
                "params": params_json,
            });
            post(&client, &format!("{}/v1/exec", cli.url), req).await?
        }
    };
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn get(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("HTTP {status}: {body}");
    }
    Ok(body)
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let resp = client.post(url).json(&payload).send().await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("HTTP {status}: {body}");
    }
    Ok(body)
}
