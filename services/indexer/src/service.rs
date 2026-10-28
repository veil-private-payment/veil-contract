//! The long-running indexer: follow RPC, report health, stop cleanly.
//!
//! One loop reads to the tip every poll interval. An HTTP server beside it
//! answers:
//!
//! - `GET /health`: the process is up (always 200).
//! - `GET /ready`: 200 when the last sync succeeded recently and the index is
//!   within `max_lag` ledgers of the RPC tip, 503 otherwise. The body says why.
//! - `GET /metrics`: a few counters and gauges in Prometheus text format.
//!
//! On SIGTERM or Ctrl-C the loop stops where it is. A page being written when
//! the signal arrives is either committed or rolled back with its checkpoint,
//! never half-stored, so stopping at any point is safe.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Serialize;
use tokio::sync::{RwLock, watch};

use crate::ingest::{Contracts, Follower, RPC_STREAM};
use crate::rpc::RpcClient;
use crate::seed::rpc_start;
use crate::store::{Store, WriterLock};

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub listen: SocketAddr,
    pub poll_interval: Duration,
    pub page_size: u32,
    pub overlap: u32,
    /// Readiness fails above this many ledgers behind the RPC tip.
    pub max_lag: u32,
    /// Readiness fails when the last successful sync is older than this.
    pub max_silence: Duration,
}

/// What `/ready` and `/metrics` report.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub latest_ledger: Option<u32>,
    pub indexed_ledger: Option<u32>,
    pub last_success: Option<Instant>,
    pub last_error: Option<String>,
    pub events_stored: u64,
    pub sync_errors: u64,
}

#[derive(Serialize)]
struct Readiness {
    ready: bool,
    reason: Option<String>,
    latest_ledger: Option<u32>,
    indexed_ledger: Option<u32>,
    lag: Option<u32>,
    last_success_secs_ago: Option<u64>,
    last_error: Option<String>,
}

#[derive(Clone)]
struct AppState {
    status: Arc<RwLock<Status>>,
    config: Arc<ServiceConfig>,
}

impl Status {
    pub fn lag(&self) -> Option<u32> {
        Some(self.latest_ledger?.saturating_sub(self.indexed_ledger?))
    }
}

fn readiness(status: &Status, config: &ServiceConfig) -> Readiness {
    let since = status.last_success.map(|at| at.elapsed());
    let lag = status.lag();
    let reason = match (since, lag) {
        (None, _) => Some("no successful sync yet".to_owned()),
        (Some(since), _) if since > config.max_silence => {
            Some(format!("last successful sync {}s ago", since.as_secs()))
        }
        (_, Some(lag)) if lag > config.max_lag => Some(format!("{lag} ledgers behind the RPC tip")),
        _ => None,
    };
    Readiness {
        ready: reason.is_none(),
        reason,
        latest_ledger: status.latest_ledger,
        indexed_ledger: status.indexed_ledger,
        lag,
        last_success_secs_ago: since.map(|since| since.as_secs()),
        last_error: status.last_error.clone(),
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let report = readiness(&*state.status.read().await, &state.config);
    let code = if report.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(report))
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.status.read().await;
    let mut out = String::new();
    let mut line = |name: &str, kind: &str, help: &str, value: Option<u64>| {
        if let Some(value) = value {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
            ));
        }
    };
    line(
        "veil_indexer_latest_ledger",
        "gauge",
        "Newest ledger RPC reported.",
        status.latest_ledger.map(u64::from),
    );
    line(
        "veil_indexer_indexed_ledger",
        "gauge",
        "Ledger the RPC checkpoint has reached.",
        status.indexed_ledger.map(u64::from),
    );
    line(
        "veil_indexer_lag_ledgers",
        "gauge",
        "Ledgers between the checkpoint and the RPC tip.",
        status.lag().map(u64::from),
    );
    line(
        "veil_indexer_events_stored_total",
        "counter",
        "Events stored for the first time by this process.",
        Some(status.events_stored),
    );
    line(
        "veil_indexer_sync_errors_total",
        "counter",
        "Sync attempts that failed.",
        Some(status.sync_errors),
    );
    line(
        "veil_indexer_last_success_age_seconds",
        "gauge",
        "Seconds since the last successful sync.",
        status.last_success.map(|at| at.elapsed().as_secs()),
    );
    ([("content-type", "text/plain; version=0.0.4")], out)
}

pub fn router(status: Arc<RwLock<Status>>, config: Arc<ServiceConfig>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .with_state(AppState { status, config })
}

/// Resolves when the process is asked to stop.
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

/// Runs until `stop` turns true. Errors are logged, reported on `/ready` and
/// retried with backoff, except a lost writer lock, which ends the run.
pub async fn run(
    rpc: &RpcClient,
    store: &Store,
    mut lock: WriterLock,
    contracts: &Contracts,
    config: Arc<ServiceConfig>,
    status: Arc<RwLock<Status>>,
    mut stop: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut failures = 0u32;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        lock.still_held().await?;

        let cycle = async {
            let range = rpc.ledger_range().await?;
            let start = rpc_start(store, range, config.overlap).await?;
            let follower = Follower {
                source: rpc,
                store,
                contracts,
                stream: RPC_STREAM,
                page_size: config.page_size,
            };
            let report = follower.sync_to_tip(start).await?;
            anyhow::Ok((range, report))
        };
        let outcome = tokio::select! {
            _ = stop.changed() => return Ok(()),
            outcome = cycle => outcome,
        };

        let delay = match outcome {
            Ok((range, report)) => {
                failures = 0;
                let mut status = status.write().await;
                status.latest_ledger = Some(range.latest_ledger);
                if let Some(checkpoint) = report.checkpoint {
                    status.indexed_ledger = Some(checkpoint.cursor.ledger());
                    status.latest_ledger = Some(range.latest_ledger.max(checkpoint.latest_ledger));
                }
                status.events_stored = status
                    .events_stored
                    .saturating_add(u64::try_from(report.inserted).unwrap_or(u64::MAX));
                status.last_success = Some(Instant::now());
                status.last_error = None;
                if report.inserted > 0 {
                    tracing::info!(
                        inserted = report.inserted,
                        indexed_ledger = status.indexed_ledger,
                        "stored new events"
                    );
                }
                config.poll_interval
            }
            Err(err) => {
                failures = failures.saturating_add(1);
                let message = format!("{err:#}");
                tracing::error!(error = %message, failures, "sync failed");
                let mut status = status.write().await;
                status.sync_errors = status.sync_errors.saturating_add(1);
                status.last_error = Some(message);
                let factor = 1u32.checked_shl(failures.min(6)).unwrap_or(64);
                config
                    .poll_interval
                    .saturating_mul(factor)
                    .min(Duration::from_secs(120))
            }
        };
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            () = tokio::time::sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServiceConfig {
        ServiceConfig {
            listen: "127.0.0.1:0".parse().expect("addr"),
            poll_interval: Duration::from_secs(5),
            page_size: 200,
            overlap: 100,
            max_lag: 10,
            max_silence: Duration::from_secs(60),
        }
    }

    #[test]
    fn not_ready_before_the_first_sync() {
        let report = readiness(&Status::default(), &config());
        assert!(!report.ready);
    }

    #[test]
    fn ready_within_the_lag_and_not_beyond_it() {
        let mut status = Status {
            latest_ledger: Some(1_000),
            indexed_ledger: Some(995),
            last_success: Some(Instant::now()),
            ..Status::default()
        };
        assert!(readiness(&status, &config()).ready);
        status.indexed_ledger = Some(989);
        let report = readiness(&status, &config());
        assert!(!report.ready);
        assert_eq!(report.lag, Some(11));
    }

    #[test]
    fn not_ready_when_syncs_stopped_succeeding() {
        let status = Status {
            latest_ledger: Some(1_000),
            indexed_ledger: Some(1_000),
            last_success: Instant::now().checked_sub(Duration::from_secs(61)),
            ..Status::default()
        };
        assert!(!readiness(&status, &config()).ready);
    }
}
