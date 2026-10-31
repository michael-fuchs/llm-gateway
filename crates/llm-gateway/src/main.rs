use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use llm_gateway::{Callers, Eur, Gateway, Mistral, Prices, UsageStore};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve(Box<ServeArgs>),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Path of the SQLite usage database; created if missing.
    #[arg(long, env = "LLM_GATEWAY_DB", default_value = "llm-gateway.db")]
    db: PathBuf,
    /// Address to listen on.
    #[arg(long, env = "LLM_GATEWAY_LISTEN", default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// Callers and their keys as `<name>=<key>`, comma-separated. Each caller sends its key
    /// as a bearer token.
    #[arg(
        long,
        env = "LLM_GATEWAY_CALLERS",
        value_delimiter = ',',
        hide_env_values = true,
        required = true
    )]
    callers: Vec<String>,
    /// Mistral API key.
    #[arg(long, env = "LLM_GATEWAY_MISTRAL_API_KEY", hide_env_values = true)]
    mistral_api_key: String,
    /// Base URL of Mistral's API.
    #[arg(long, env = "LLM_GATEWAY_MISTRAL_URL", default_value = Mistral::DEFAULT_URL)]
    mistral_url: String,
    /// Monthly budget for all callers together, in euros. A calendar month in UTC.
    #[arg(long, env = "LLM_GATEWAY_BUDGET_EUR", default_value = llm_gateway::DEFAULT_BUDGET)]
    budget_eur: Eur,
    /// Offered models and their prices as `<model>=<euros per million input tokens>`,
    /// comma-separated. Requests for other models are refused.
    #[arg(
        long,
        env = "LLM_GATEWAY_PRICES",
        value_delimiter = ',',
        default_value = llm_gateway::DEFAULT_PRICES
    )]
    prices: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Serve(args) => serve(*args).await,
    }
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let callers = Callers::parse(&args.callers).map_err(anyhow::Error::msg)?;
    if callers.is_empty() {
        bail!("no callers are configured");
    }
    let prices = Prices::parse(&args.prices).map_err(anyhow::Error::msg)?;
    let store = UsageStore::open(&args.db)
        .with_context(|| format!("opening the usage database {}", args.db.display()))?;
    tracing::info!(?callers, budget = %args.budget_eur, "configured");

    let gateway = Arc::new(Gateway {
        callers,
        prices,
        budget: args.budget_eur,
        provider: Arc::new(
            Mistral::new(args.mistral_api_key, &args.mistral_url)
                .context("creating the Mistral client")?,
        ),
        store,
        clock: llm_gateway::system_clock(),
    });

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    tracing::info!(listen = %args.listen, "serving");
    axum::serve(listener, llm_gateway::api::router(gateway))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM (what a container runtime sends to stop).
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
