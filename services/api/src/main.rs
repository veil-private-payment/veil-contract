use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use veil_api::routes::{ApiConfig, AppState, app};
use veil_indexer::config::{LogConfig, Secret, parse_contract_id};
use veil_indexer::logging;

/// Read-only HTTP API over the Veil indexer's database.
#[derive(Debug, Parser)]
#[command(name = "veil-api", version, about)]
struct Cli {
    #[command(flatten)]
    log: LogConfig,

    /// Postgres URL of the indexer's database. Every session the API opens is
    /// made read-only.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,

    #[arg(long, env = "VEIL_DB_MAX_CONNECTIONS", default_value_t = 10)]
    db_max_connections: u32,

    #[arg(
        long,
        env = "VEIL_NETWORK_PASSPHRASE",
        default_value = "Test SDF Network ; September 2015"
    )]
    network_passphrase: String,

    #[arg(long, env = "VEIL_POOL_CONTRACT", value_parser = parse_contract_id)]
    pool_contract: String,

    #[arg(long, env = "VEIL_ALLOWLIST_CONTRACT", value_parser = parse_contract_id)]
    allowlist_contract: String,

    /// Address to listen on. `PORT`, as hosting platforms set it, overrides
    /// the port.
    #[arg(long, env = "VEIL_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,

    #[arg(long, env = "PORT")]
    port: Option<u16>,

    /// Sustained requests per second per client address.
    #[arg(long, env = "VEIL_RATE_PER_SECOND", default_value_t = 20)]
    rate_per_second: u64,

    /// Requests a client may burst above the sustained rate.
    #[arg(long, env = "VEIL_RATE_BURST", default_value_t = 100)]
    rate_burst: u32,

    /// Seconds a request may take before it is answered with 408.
    #[arg(long, env = "VEIL_REQUEST_TIMEOUT_SECS", default_value_t = 10)]
    request_timeout_secs: u64,

    /// `/ready` fails when the indexer has not advanced for this many seconds.
    #[arg(long, env = "VEIL_READY_MAX_STALENESS_SECS", default_value_t = 120)]
    ready_max_staleness_secs: u64,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.log.log_format);
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "veil-api failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let url = Secret::new(cli.database_url);
    let options = url
        .expose()
        .parse::<sqlx::postgres::PgConnectOptions>()
        .context("parsing DATABASE_URL")?;
    let db = veil_api::read_only_pool(options, cli.db_max_connections)
        .await
        .context("connecting to the database")?;

    let mut listen = cli.listen;
    if let Some(port) = cli.port {
        listen.set_port(port);
    }
    let state = AppState {
        db: db.clone(),
        config: Arc::new(ApiConfig {
            network_passphrase: cli.network_passphrase,
            pool: cli.pool_contract,
            allowlist: cli.allowlist_contract,
            max_staleness: Duration::from_secs(cli.ready_max_staleness_secs),
            request_timeout: Duration::from_secs(cli.request_timeout_secs),
            rate_limit: Some((cli.rate_per_second, cli.rate_burst)),
        }),
    };
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, "serving the read API");
    axum::serve(
        listener,
        app(state)?.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(veil_indexer::service::shutdown_signal())
    .await
    .context("serving")?;
    db.close().await;
    tracing::info!("stopped");
    Ok(())
}
