use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use std::time::Duration;

use llm_gateway::{
    Callers, Eur, Gateway, Mistral, Prices, RateLimit, RetryPolicy, Spheres, UsageStore,
};
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
    /// The spheres and their rules, as rendered from the assistant's central configuration.
    /// Requests for spheres not declared there are refused.
    #[arg(long, env = "LLM_GATEWAY_SPHERES")]
    spheres: PathBuf,
    /// Texts each caller may send for embedding per minute; also the largest batch.
    #[arg(long, env = "LLM_GATEWAY_INPUTS_PER_MINUTE", default_value_t = llm_gateway::limits::DEFAULT_INPUTS_PER_MINUTE, value_parser = clap::value_parser!(u32).range(1..))]
    inputs_per_minute: u32,
    /// The longest a provider call may take in seconds, retries included; then the request
    /// fails with provider_failure.
    #[arg(long, env = "LLM_GATEWAY_PROVIDER_TIME_LIMIT", default_value_t = RetryPolicy::default().time_limit.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
    provider_time_limit: u64,
    /// The longest a single attempt waits for the provider's answer, in seconds.
    #[arg(long, env = "LLM_GATEWAY_PROVIDER_ATTEMPT_TIMEOUT", default_value_t = RetryPolicy::default().attempt_timeout.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
    provider_attempt_timeout: u64,
    /// Address for the owner's usage view, a separate listener so that only it is exposed to
    /// browsers. Without it, there is no usage view.
    #[arg(long, env = "LLM_GATEWAY_WEB_LISTEN")]
    web_listen: Option<SocketAddr>,
    /// Issuer of the owner's ID tokens (the OIDC provider), exactly as in their `iss` claim.
    /// Without it, the usage view has no login and is only allowed on a loopback address.
    #[arg(long, env = "LLM_GATEWAY_OIDC_ISSUER")]
    oidc_issuer: Option<String>,
    /// The URL under which the owner's browser reaches the usage view; the login returns to
    /// `<public-url>/auth/callback`.
    #[arg(
        long,
        env = "LLM_GATEWAY_PUBLIC_URL",
        default_value = "http://localhost:8081"
    )]
    public_url: String,
    /// Client ID of the usage view at the OIDC provider.
    #[arg(
        long,
        env = "LLM_GATEWAY_WEB_CLIENT_ID",
        default_value = "llm-gateway-web"
    )]
    web_client_id: String,
    /// Secret of the usage view's client, if it is confidential; a public client uses PKCE
    /// alone.
    #[arg(long, env = "LLM_GATEWAY_WEB_CLIENT_SECRET", hide_env_values = true)]
    web_client_secret: Option<String>,
    /// Only members of this group at the OIDC provider may log in to the usage view.
    #[arg(
        long,
        env = "LLM_GATEWAY_OWNER_GROUP",
        default_value = "llm_gateway_owner"
    )]
    owner_group: String,
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
    let spheres = Spheres::load(&args.spheres).map_err(anyhow::Error::msg)?;
    let mut sphere_names: Vec<_> = spheres.names().collect();
    sphere_names.sort_unstable();
    let store = UsageStore::open(&args.db)
        .with_context(|| format!("opening the usage database {}", args.db.display()))?;
    tracing::info!(?callers, spheres = ?sphere_names, budget = %args.budget_eur, "configured");

    let gateway = Arc::new(Gateway {
        callers,
        prices,
        budget: args.budget_eur,
        spheres,
        rate_limit: RateLimit::new(args.inputs_per_minute),
        retry: RetryPolicy {
            time_limit: Duration::from_secs(args.provider_time_limit),
            attempt_timeout: Duration::from_secs(args.provider_attempt_timeout),
            ..RetryPolicy::default()
        },
        provider: Arc::new(
            Mistral::new(args.mistral_api_key.clone(), &args.mistral_url)
                .context("creating the Mistral client")?,
        ),
        store,
        clock: llm_gateway::system_clock(),
    });

    let web = match args.web_listen {
        Some(web_listen) => {
            let login = web_login(&args, web_listen).await?;
            let listener = tokio::net::TcpListener::bind(web_listen)
                .await
                .with_context(|| format!("binding {web_listen}"))?;
            tracing::info!(listen = %web_listen, "serving the usage view");
            Some((listener, llm_gateway::web::router(gateway.clone(), login)))
        }
        None => None,
    };

    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    tracing::info!(listen = %args.listen, "serving the API");

    let (stop, stopped) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        shutdown().await;
        let _ = stop.send(());
    });
    let until_stopped = |mut stopped: tokio::sync::watch::Receiver<()>| async move {
        let _ = stopped.changed().await;
    };
    let api = axum::serve(listener, llm_gateway::api::router(gateway))
        .with_graceful_shutdown(until_stopped(stopped.clone()));
    match web {
        Some((listener, router)) => {
            let web = axum::serve(listener, router).with_graceful_shutdown(until_stopped(stopped));
            tokio::try_join!(api.into_future(), web.into_future())?;
        }
        None => api.await?,
    }
    Ok(())
}

/// The owner's login for the usage view, or `None` for development on a loopback address.
async fn web_login(
    args: &ServeArgs,
    web_listen: SocketAddr,
) -> anyhow::Result<Option<llm_gateway::web::LoginSettings>> {
    let Some(issuer) = &args.oidc_issuer else {
        if !web_listen.ip().is_loopback() {
            bail!(
                "refusing to serve the usage view on {web_listen} without a login: set --oidc-issuer or use a loopback address"
            );
        }
        tracing::warn!("the usage view has no login; anyone on this machine can see it");
        return Ok(None);
    };
    let provider = llm_gateway::web::oidc::Provider::discover(issuer)
        .await
        .with_context(|| format!("discovering the OIDC provider {issuer}"))?;
    let public_url = args.public_url.trim_end_matches('/');
    Ok(Some(llm_gateway::web::LoginSettings {
        provider: Arc::new(provider),
        client_id: args.web_client_id.clone(),
        client_secret: args.web_client_secret.clone(),
        redirect_uri: format!("{public_url}/auth/callback"),
        owner_group: args.owner_group.clone(),
        secure_cookies: public_url.starts_with("https://"),
    }))
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
