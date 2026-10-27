use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use veil_indexer::config::{ChainConfig, DatabaseConfig, IngestConfig, LogConfig};
use veil_indexer::event::Decoded;
use veil_indexer::ingest::{Contracts, Follower, RPC_STREAM};
use veil_indexer::rpc::{Retry, RpcClient};
use veil_indexer::source::{ExpertEventRecord, Source};
use veil_indexer::store::{Store, StoreOptions};
use veil_indexer::{logging, position::EventPosition};

#[derive(Debug, Parser)]
#[command(name = "veil-indexer", version, about)]
struct Cli {
    #[command(flatten)]
    log: LogConfig,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a saved Stellar Expert event listing and print one JSON line per
    /// event. Any event that does not match the contract source is an error.
    Decode {
        /// JSON array of records from Stellar Expert's contract events endpoint.
        file: PathBuf,
    },
    /// Validate the configuration and print it.
    Config {
        #[command(flatten)]
        chain: ChainConfig,
    },
    /// Apply database migrations and exit.
    Migrate {
        #[command(flatten)]
        database: DatabaseConfig,
    },
    /// Read events from RPC until the tip, store them, and exit. Resumes from
    /// the stored checkpoint.
    Sync {
        #[command(flatten)]
        chain: ChainConfig,
        #[command(flatten)]
        database: DatabaseConfig,
        #[command(flatten)]
        ingest: IngestConfig,
    },
}

#[derive(Serialize)]
struct DecodedLine<'a> {
    position: String,
    ledger: u32,
    contract: &'a str,
    source: Source,
    #[serde(flatten)]
    decoded: DecodedBody<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum DecodedBody<'a> {
    Event {
        event: &'a veil_indexer::event::VeilEvent,
    },
    Ignored {
        ignored: &'a str,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.log.log_format);
    match run(cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "command failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> Result<()> {
    match command {
        Command::Decode { file } => decode(&file),
        Command::Config { chain } => {
            tracing::info!(?chain, "configuration is valid");
            Ok(())
        }
        Command::Migrate { database } => {
            let store = connect(&database).await?;
            store.migrate().await.context("applying migrations")?;
            tracing::info!("migrations applied");
            Ok(())
        }
        Command::Sync {
            chain,
            database,
            ingest,
        } => sync(chain, database, ingest).await,
    }
}

async fn connect(database: &DatabaseConfig) -> Result<Store> {
    let options = StoreOptions {
        max_connections: database.db_max_connections,
        ..StoreOptions::default()
    };
    Store::connect(&database.database_url, &options)
        .await
        .context("connecting to the database")
}

async fn sync(chain: ChainConfig, database: DatabaseConfig, ingest: IngestConfig) -> Result<()> {
    let store = connect(&database).await?;
    store.migrate().await.context("applying migrations")?;
    let rpc = RpcClient::new(
        &chain.rpc_url,
        Duration::from_secs(ingest.rpc_timeout_secs),
        Retry::default(),
    )
    .context("building the RPC client")?;
    let contracts = Contracts {
        pool: chain.pool_contract,
        allowlist: chain.allowlist_contract,
    };
    let follower = Follower {
        source: &rpc,
        store: &store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: ingest.page_size,
    };
    let report = follower.sync_to_tip(ingest.start_ledger).await?;
    tracing::info!(
        pages = report.pages,
        inserted = report.inserted,
        already_stored = report.already_stored,
        ignored = report.ignored,
        cursor = report.checkpoint.map(|c| c.cursor.to_string()),
        latest_ledger = report.checkpoint.map(|c| c.latest_ledger),
        "reached the tip"
    );
    Ok(())
}

fn decode(file: &PathBuf) -> Result<()> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let records: Vec<ExpertEventRecord> =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;

    let mut stdout = io::stdout().lock();
    let mut previous: Option<EventPosition> = None;
    for record in records {
        let event = record.into_event()?;
        if previous.is_some_and(|prev| prev >= event.position) {
            anyhow::bail!("events are not in ledger order at {}", event.position);
        }
        previous = Some(event.position);

        let decoded = event
            .raw
            .decode()
            .with_context(|| format!("decoding event {}", event.position))?;
        let body = match &decoded {
            Decoded::Event(event) => DecodedBody::Event { event },
            Decoded::Ignored { name } => DecodedBody::Ignored { ignored: name },
        };
        let line = DecodedLine {
            position: event.position.to_string(),
            ledger: event.position.ledger(),
            contract: &event.contract,
            source: event.source,
            decoded: body,
        };
        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        match stdout.write_all(&bytes) {
            // The reader closed the pipe, e.g. `| head`. Stop quietly.
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            other => other?,
        }
    }
    Ok(())
}
