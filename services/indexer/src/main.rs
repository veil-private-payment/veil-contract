use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use veil_indexer::config::{ChainConfig, LogConfig};
use veil_indexer::event::Decoded;
use veil_indexer::source::{ExpertEventRecord, Source};
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.log.log_format);
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "command failed");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Decode { file } => decode(&file),
        Command::Config { chain } => {
            tracing::info!(?chain, "configuration is valid");
            Ok(())
        }
    }
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
