use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::sync::Arc;

use tokio::sync::{RwLock, watch};
use veil_indexer::config::{
    ChainConfig, DatabaseConfig, ExpertConfig, IngestConfig, LogConfig, ServiceArgs,
};
use veil_indexer::event::Decoded;
use veil_indexer::expert::ExpertClient;
use veil_indexer::ingest::{Contracts, Follower, RPC_STREAM};
use veil_indexer::rpc::{Retry, RpcClient};
use veil_indexer::seed::rpc_start;
use veil_indexer::service::{ServiceConfig, Status};
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
    /// Read both contracts' full history from Stellar Expert into the
    /// database. Run once before `run`; resumable.
    Seed {
        #[command(flatten)]
        chain: ChainConfig,
        #[command(flatten)]
        database: DatabaseConfig,
        #[command(flatten)]
        expert: ExpertConfig,
        #[command(flatten)]
        ingest: IngestConfig,
    },
    /// Compare the index with the contracts' on-chain roots and tree sizes.
    /// Exits non-zero on any mismatch.
    Verify {
        #[command(flatten)]
        chain: ChainConfig,
        #[command(flatten)]
        database: DatabaseConfig,
        #[command(flatten)]
        ingest: IngestConfig,
    },
    /// Follow RPC continuously and serve /health, /ready and /metrics.
    /// Seeds first if the database is empty.
    Run {
        #[command(flatten)]
        chain: ChainConfig,
        #[command(flatten)]
        expert: ExpertConfig,
        #[command(flatten)]
        database: DatabaseConfig,
        #[command(flatten)]
        ingest: IngestConfig,
        #[command(flatten)]
        service: ServiceArgs,
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
        Command::Seed {
            chain,
            database,
            expert,
            ingest,
        } => seed(chain, database, expert, ingest).await,
        Command::Verify {
            chain,
            database,
            ingest,
        } => verify(chain, database, ingest).await,
        Command::Run {
            chain,
            expert,
            database,
            ingest,
            service,
        } => serve(chain, expert, database, ingest, service).await,
    }
}

fn rpc_client(chain: &ChainConfig, ingest: &IngestConfig) -> Result<RpcClient> {
    RpcClient::new(
        &chain.rpc_url,
        Duration::from_secs(ingest.rpc_timeout_secs),
        Retry::default(),
    )
    .context("building the RPC client")
}

fn contracts(chain: &ChainConfig) -> Contracts {
    Contracts {
        pool: chain.pool_contract.clone(),
        allowlist: chain.allowlist_contract.clone(),
    }
}

async fn seed(
    chain: ChainConfig,
    database: DatabaseConfig,
    expert: ExpertConfig,
    ingest: IngestConfig,
) -> Result<()> {
    let store = connect(&database).await?;
    store.migrate().await.context("applying migrations")?;
    let _lock = store.writer_lock().await?;
    let client = ExpertClient::new(
        &expert.expert_url,
        Duration::from_secs(ingest.rpc_timeout_secs),
        Retry::default(),
    )
    .context("building the Stellar Expert client")?;
    // Stellar Expert pages at most 200 events.
    let report = veil_indexer::seed::seed(
        &client,
        &store,
        &contracts(&chain),
        ingest.page_size.min(200),
    )
    .await?;
    tracing::info!(
        pages = report.pages,
        inserted = report.inserted,
        already_stored = report.already_stored,
        complete_to = report.complete_to,
        "seed complete"
    );
    Ok(())
}

async fn verify(chain: ChainConfig, database: DatabaseConfig, ingest: IngestConfig) -> Result<()> {
    let store = connect(&database).await?;
    let rpc = rpc_client(&chain, &ingest)?;
    let contracts = contracts(&chain);
    // With no indexer running, catch up first. With one running, it holds
    // the writer lock and verify waits for it instead.
    match store.writer_lock().await {
        Ok(lock) => {
            let range = rpc.ledger_range().await?;
            let start = rpc_start(&store, range, ingest.overlap_ledgers).await?;
            Follower {
                source: &rpc,
                store: &store,
                contracts: &contracts,
                stream: RPC_STREAM,
                page_size: ingest.page_size,
            }
            .sync_to_tip(start)
            .await?;
            drop(lock);
        }
        Err(veil_indexer::store::StoreError::Locked) => {
            tracing::info!("an indexer is running, waiting for it to reach the state's ledger");
        }
        Err(err) => return Err(err.into()),
    }
    let report =
        veil_indexer::verify::verify(&rpc, &store, &contracts, Duration::from_secs(60)).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    for check in &report.checks {
        if check.ok() {
            tracing::info!(check = check.name, value = check.indexed, "match");
        } else {
            tracing::error!(
                check = check.name,
                indexed = check.indexed,
                on_chain = check.on_chain,
                "MISMATCH"
            );
        }
    }
    anyhow::ensure!(report.ok(), "the index does not match the chain");
    Ok(())
}

async fn serve(
    chain: ChainConfig,
    expert: ExpertConfig,
    database: DatabaseConfig,
    ingest: IngestConfig,
    args: ServiceArgs,
) -> Result<()> {
    let store = connect(&database).await?;
    store.migrate().await.context("applying migrations")?;
    let rpc = rpc_client(&chain, &ingest)?;
    let config = Arc::new(ServiceConfig {
        listen: args.listen,
        poll_interval: Duration::from_secs(args.poll_secs.max(1)),
        page_size: ingest.page_size,
        overlap: ingest.overlap_ledgers,
        max_lag: args.ready_max_lag,
        max_silence: Duration::from_secs(args.ready_max_silence_secs),
        seed_from: if args.seed_if_empty {
            Some(
                ExpertClient::new(
                    &expert.expert_url,
                    Duration::from_secs(ingest.rpc_timeout_secs),
                    Retry::default(),
                )
                .context("building the Stellar Expert client")?,
            )
        } else {
            None
        },
    });
    let status = Arc::new(RwLock::new(Status::default()));
    let (stop_tx, stop_rx) = watch::channel(false);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!(listen = %config.listen, "serving /health, /ready, /metrics");
    let mut server_stop = stop_rx.clone();
    let server = tokio::spawn(
        axum::serve(
            listener,
            veil_indexer::service::router(status.clone(), config.clone()),
        )
        .with_graceful_shutdown(async move {
            let _ = server_stop.wait_for(|stop| *stop).await;
        })
        .into_future(),
    );
    tokio::spawn(async move {
        veil_indexer::service::shutdown_signal().await;
        tracing::info!("shutting down");
        let _ = stop_tx.send(true);
    });

    // Serve /health first, then wait for any previous instance to let go.
    let mut waiting = stop_rx.clone();
    let lock =
        veil_indexer::service::acquire_writer_lock(&store, Duration::from_secs(5), &mut waiting)
            .await?;
    let Some(lock) = lock else {
        server.abort();
        return Ok(());
    };
    let contracts = contracts(&chain);
    let verifier = (args.verify_every_secs > 0).then(|| {
        let (rpc, store, contracts, status, stop) = (
            rpc.clone(),
            store.clone(),
            contracts.clone(),
            status.clone(),
            stop_rx.clone(),
        );
        let every = Duration::from_secs(args.verify_every_secs);
        tokio::spawn(async move {
            veil_indexer::service::verify_periodically(
                &rpc, &store, &contracts, every, status, stop,
            )
            .await;
        })
    });
    let outcome =
        veil_indexer::service::run(&rpc, &store, lock, &contracts, config, status, stop_rx).await;
    if let Some(verifier) = verifier {
        verifier.abort();
    }
    server.abort();
    store.pool().close().await;
    outcome
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
    let _lock = store.writer_lock().await?;
    let rpc = rpc_client(&chain, &ingest)?;
    let contracts = contracts(&chain);
    let start = match ingest.start_ledger {
        Some(ledger) => Some(ledger),
        None => {
            let range = rpc.ledger_range().await?;
            rpc_start(&store, range, ingest.overlap_ledgers).await?
        }
    };
    let follower = Follower {
        source: &rpc,
        store: &store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: ingest.page_size,
    };
    let report = follower.sync_to_tip(start).await?;
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
