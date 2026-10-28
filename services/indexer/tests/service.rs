//! The long-running service: the single-writer lock, the HTTP endpoints and
//! the follow loop, against a real Postgres and a mock RPC.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TestDb, contracts};
use serde_json::json;
use tokio::sync::{RwLock, watch};
use veil_indexer::ingest::RPC_STREAM;
use veil_indexer::rpc::{Retry, RpcClient};
use veil_indexer::seed::{SEED_STREAM, end_of_ledger};
use veil_indexer::service::{ServiceConfig, Status, router, run};
use veil_indexer::store::{Checkpoint, StoreError};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TIP: u32 = 5_428_200;

fn config() -> ServiceConfig {
    ServiceConfig {
        listen: "127.0.0.1:0".parse().expect("addr"),
        poll_interval: Duration::from_millis(50),
        page_size: 200,
        overlap: 100,
        max_lag: 10,
        max_silence: Duration::from_secs(60),
    }
}

fn rpc(server: &MockServer) -> RpcClient {
    let retry = Retry {
        attempts: 1,
        first_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    RpcClient::new(&server.uri(), Duration::from_secs(5), retry).expect("client")
}

#[tokio::test]
async fn only_one_process_holds_the_writer_lock() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let other = db.second_store().await;

    let mut lock = db.store.writer_lock().await.expect("first");
    lock.still_held().await.expect("held");
    let error = other.writer_lock().await.err().expect("second is refused");
    assert!(matches!(error, StoreError::Locked), "{error}");

    drop(lock);
    // Postgres releases the lock once it sees the connection close.
    let started = Instant::now();
    loop {
        match other.writer_lock().await {
            Ok(_) => break,
            Err(StoreError::Locked) if started.elapsed() < Duration::from_secs(5) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("lock not released: {err}"),
        }
    }
    db.drop().await;
}

async fn serve(status: Arc<RwLock<Status>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(axum::serve(listener, router(status, Arc::new(config()))).into_future());
    format!("http://{address}")
}

#[tokio::test]
async fn the_endpoints_report_readiness_and_counters() {
    let status = Arc::new(RwLock::new(Status::default()));
    let base = serve(status.clone()).await;
    let http = reqwest::Client::new();

    let health = http
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("health");
    assert_eq!(health.status(), 200);
    assert_eq!(health.text().await.expect("body"), "ok");

    let ready = http
        .get(format!("{base}/ready"))
        .send()
        .await
        .expect("ready");
    assert_eq!(ready.status(), 503);
    let body: serde_json::Value = ready.json().await.expect("json");
    assert_eq!(body["ready"], false);
    assert_eq!(body["reason"], "no successful sync yet");

    *status.write().await = Status {
        latest_ledger: Some(TIP),
        indexed_ledger: Some(TIP.saturating_sub(3)),
        last_success: Some(Instant::now()),
        events_stored: 74,
        sync_errors: 2,
        ..Status::default()
    };
    let ready = http
        .get(format!("{base}/ready"))
        .send()
        .await
        .expect("ready");
    assert_eq!(ready.status(), 200);
    let body: serde_json::Value = ready.json().await.expect("json");
    assert_eq!(body["lag"], 3);

    let metrics = http
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("body");
    for line in [
        "veil_indexer_lag_ledgers 3",
        "veil_indexer_events_stored_total 74",
        "veil_indexer_sync_errors_total 2",
        &format!("veil_indexer_latest_ledger {TIP}"),
    ] {
        assert!(
            metrics.lines().any(|l| l == line),
            "missing {line:?} in\n{metrics}"
        );
    }
}

/// An RPC at its tip with no new events.
async fn quiet_rpc() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "getHealth" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "status": "healthy", "latestLedger": TIP, "oldestLedger": TIP - 100_000 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "getEvents" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "events": [],
                "cursor": end_of_ledger(TIP).to_string(),
                "latestLedger": TIP,
                "oldestLedger": TIP - 100_000
            }
        })))
        .mount(&server)
        .await;
    server
}

async fn wait_for(status: &Arc<RwLock<Status>>, done: impl Fn(&Status) -> bool) {
    let started = Instant::now();
    while !done(&*status.read().await) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed out: {:?}",
            status.read().await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_loop_hands_off_from_the_seed_reaches_the_tip_and_stops_on_request() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    db.store
        .write_batch(
            SEED_STREAM,
            &[],
            Checkpoint {
                cursor: end_of_ledger(TIP - 50),
                latest_ledger: TIP - 50,
            },
        )
        .await
        .expect("seeded");
    let server = quiet_rpc().await;
    let rpc = rpc(&server);
    let status = Arc::new(RwLock::new(Status::default()));
    let (stop, stopped) = watch::channel(false);
    let lock = db.store.writer_lock().await.expect("lock");

    let store = db.store.clone();
    let task_status = status.clone();
    let task = tokio::spawn(async move {
        run(
            &rpc,
            &store,
            lock,
            &contracts(),
            Arc::new(config()),
            task_status,
            stopped,
        )
        .await
    });

    wait_for(&status, |s| s.indexed_ledger == Some(TIP)).await;
    assert_eq!(status.read().await.lag(), Some(0));
    // The first request started inside the overlap before the seed's end.
    let first = server
        .received_requests()
        .await
        .expect("recorded")
        .into_iter()
        .find_map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
            (body["method"] == "getEvents").then(|| body["params"]["startLedger"].clone())
        })
        .expect("a getEvents request");
    assert_eq!(first, json!(TIP - 150));

    stop.send(true).expect("signal");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stops promptly")
        .expect("joins")
        .expect("clean exit");
    assert_eq!(
        db.store
            .checkpoint(RPC_STREAM)
            .await
            .expect("read")
            .map(|c| c.cursor),
        Some(end_of_ledger(TIP))
    );
    db.drop().await;
}

#[tokio::test]
async fn failures_are_reported_and_retried_without_ending_the_loop() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // Nothing seeded: every cycle fails the hand-off.
    let server = quiet_rpc().await;
    let rpc = rpc(&server);
    let status = Arc::new(RwLock::new(Status::default()));
    let (stop, stopped) = watch::channel(false);
    let lock = db.store.writer_lock().await.expect("lock");

    let store = db.store.clone();
    let task_status = status.clone();
    let task = tokio::spawn(async move {
        run(
            &rpc,
            &store,
            lock,
            &contracts(),
            Arc::new(config()),
            task_status,
            stopped,
        )
        .await
    });
    wait_for(&status, |s| s.sync_errors >= 2).await;
    let error = status
        .read()
        .await
        .last_error
        .clone()
        .expect("error recorded");
    assert!(error.contains("seed"), "{error}");
    assert!(!task.is_finished());

    stop.send(true).expect("signal");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stops promptly")
        .expect("joins")
        .expect("clean exit");
    db.drop().await;
}
