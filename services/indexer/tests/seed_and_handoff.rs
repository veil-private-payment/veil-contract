//! The one-time seed from Stellar Expert and the hand-off to RPC, on a real
//! Postgres. Stellar Expert is a mock server that pages the recorded testnet
//! listings by cursor the way the real API does.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{ALLOWLIST, FakeRpc, POOL, TestDb, contracts, history};
use serde_json::{Value, json};
use stellar_xdr::curr::{ScBytes, ScVal};
use veil_indexer::chain::LedgerRange;
use veil_indexer::event::{Decoded, VeilEvent};
use veil_indexer::expert::ExpertClient;
use veil_indexer::ingest::{Follower, IngestError, RPC_STREAM};
use veil_indexer::rpc::Retry;
use veil_indexer::seed::{HandoffError, SEED_STREAM, SeedError, end_of_ledger, rpc_start, seed};
use veil_indexer::store::{Checkpoint, StoreError};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Newest ledger the mock Stellar Expert reports, after all of the history.
const EXPERT_TIP: u32 = 5_428_111;

fn listing(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/tests/fixtures/stellar_expert/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON")
}

fn query(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// Answers `/contract/{id}/events` from `listings`, after `cursor`, at most
/// `limit` records, oldest first. `fail_from` makes every request from that
/// one on fail with 503.
#[derive(Clone)]
struct FakeExpert {
    listings: Arc<Vec<(String, Vec<Value>)>>,
    served: Arc<AtomicUsize>,
    fail_from: Option<usize>,
}

impl wiremock::Respond for FakeExpert {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let served = self.served.fetch_add(1, Ordering::SeqCst);
        if self.fail_from.is_some_and(|from| served >= from) {
            return ResponseTemplate::new(503);
        }
        let contract = request
            .url
            .path_segments()
            .and_then(|mut segments| segments.nth(3).map(str::to_owned))
            .expect("contract in path");
        let limit: usize = query(request, "limit")
            .and_then(|limit| limit.parse().ok())
            .unwrap_or(10);
        let cursor = query(request, "cursor");
        let records: Vec<Value> = self
            .listings
            .iter()
            .find(|(id, _)| *id == contract)
            .map(|(_, records)| records.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|record| {
                cursor
                    .as_deref()
                    .is_none_or(|cursor| record["id"].as_str().expect("id") > cursor)
            })
            .take(limit)
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({ "_embedded": { "records": records } }))
    }
}

async fn expert(
    listings: Vec<(String, Vec<Value>)>,
    fail_from: Option<usize>,
) -> (MockServer, Arc<AtomicUsize>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/explorer/testnet/ledger/last"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "sequence": EXPERT_TIP })))
        .mount(&server)
        .await;
    let served = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/explorer/testnet/contract/C[A-Z2-7]{55}/events$",
        ))
        .respond_with(FakeExpert {
            listings: Arc::new(listings),
            served: served.clone(),
            fail_from,
        })
        .mount(&server)
        .await;
    (server, served)
}

fn real_listings() -> Vec<(String, Vec<Value>)> {
    vec![
        (POOL.to_owned(), listing("pool_CDJELV6H.json")),
        (ALLOWLIST.to_owned(), listing("asp_CBRZ7RD5.json")),
    ]
}

fn client(server: &MockServer) -> ExpertClient {
    let retry = Retry {
        attempts: 1,
        first_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    ExpertClient::new(
        &format!("{}/explorer/testnet", server.uri()),
        Duration::from_secs(5),
        retry,
    )
    .expect("client")
}

/// Rows of the typed tables only, which carry no source column, so a seeded
/// database and an RPC-filled one can be compared.
async fn typed_rows(db: &TestDb) -> Vec<String> {
    db.dump()
        .await
        .into_iter()
        .filter(|row| !row.starts_with("contract_events:"))
        .collect()
}

fn range(oldest: u32) -> LedgerRange {
    LedgerRange {
        latest_ledger: EXPERT_TIP.saturating_add(100),
        oldest_ledger: oldest,
    }
}

#[tokio::test]
async fn the_seed_stores_the_full_history_and_marks_where_it_is_complete() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let (server, _) = expert(real_listings(), None).await;

    let report = seed(&client(&server), &db.store, &contracts(), 10)
        .await
        .expect("seed");
    assert_eq!(report.inserted, 74);
    assert_eq!(report.complete_to, EXPERT_TIP);
    // 69 pool events in pages of 10 (7 pages), 5 allowlist events (1 page).
    assert_eq!(report.pages, 8);

    let complete = db
        .store
        .checkpoint(SEED_STREAM)
        .await
        .expect("read")
        .expect("set");
    assert_eq!(complete.cursor, end_of_ledger(EXPERT_TIP));
    let source: Vec<String> = sqlx::query_scalar("SELECT DISTINCT source FROM contract_events")
        .fetch_all(db.store.pool())
        .await
        .expect("read");
    assert_eq!(source, ["stellar_expert"]);

    // The same rows an RPC replay of the history produces.
    let Some(from_rpc) = TestDb::migrated().await else {
        return;
    };
    let contracts = contracts();
    Follower {
        source: &FakeRpc::new(history()),
        store: &from_rpc.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 50,
    }
    .sync_to_tip(Some(1))
    .await
    .expect("rpc replay");
    assert_eq!(typed_rows(&db).await, typed_rows(&from_rpc).await);
    db.drop().await;
    from_rpc.drop().await;
}

#[tokio::test]
async fn an_interrupted_seed_resumes_from_its_last_page() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let (failing, _) = expert(real_listings(), Some(3)).await;
    let error = seed(&client(&failing), &db.store, &contracts(), 10)
        .await
        .expect_err("fails");
    assert!(matches!(error, SeedError::Expert(_)), "{error}");
    assert_eq!(db.count("contract_events").await, 30);
    assert_eq!(db.store.checkpoint(SEED_STREAM).await.expect("read"), None);

    let (server, _) = expert(real_listings(), None).await;
    let report = seed(&client(&server), &db.store, &contracts(), 10)
        .await
        .expect("resumed");
    assert_eq!(report.inserted, 44);
    assert_eq!(report.already_stored, 0);
    let first_events_request = server
        .received_requests()
        .await
        .expect("recorded")
        .into_iter()
        .find(|request| request.url.path().ends_with("/events"))
        .expect("an events request");
    assert!(
        query(&first_events_request, "cursor").is_some(),
        "the resumed seed started over"
    );
    assert_eq!(db.count("contract_events").await, 74);
    db.drop().await;
}

#[tokio::test]
async fn rpc_rereads_the_overlap_and_finds_the_same_events() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let (server, _) = expert(real_listings(), None).await;
    seed(&client(&server), &db.store, &contracts(), 200)
        .await
        .expect("seed");

    // An overlap reaching back over the whole history.
    let start = rpc_start(&db.store, range(1), EXPERT_TIP)
        .await
        .expect("hand-off");
    assert_eq!(start, Some(1));
    let contracts = contracts();
    let report = Follower {
        source: &FakeRpc::new(history()),
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 20,
    }
    .sync_to_tip(start)
    .await
    .expect("rpc");
    assert_eq!(report.inserted, 0);
    assert_eq!(report.already_stored, 74);
    db.drop().await;
}

#[tokio::test]
async fn rpc_disagreeing_with_the_seed_stops_the_hand_off() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let (server, _) = expert(real_listings(), None).await;
    seed(&client(&server), &db.store, &contracts(), 200)
        .await
        .expect("seed");

    // RPC's copy of a commitment carries a note that differs in one byte:
    // it still decodes, so only the comparison with the seeded copy catches it.
    let mut events = history();
    let target = events
        .iter()
        .position(|event| {
            matches!(
                event.raw.decode(),
                Ok(Decoded::Event(VeilEvent::NewCommitment { ref encrypted_output, .. }))
                    if !encrypted_output.is_empty()
            )
        })
        .expect("a commitment with a note");
    let ScVal::Map(Some(fields)) = &mut events[target].raw.data else {
        panic!("event body is a map");
    };
    let flipped = fields.0.iter_mut().any(|entry| match &mut entry.val {
        ScVal::Bytes(bytes) if !bytes.0.is_empty() => {
            let mut raw = bytes.0.to_vec();
            raw[0] ^= 0xff;
            *bytes = ScBytes(raw.try_into().expect("fits"));
            true
        }
        _ => false,
    });
    assert!(flipped);
    let position = events[target].position;

    let contracts = contracts();
    let error = Follower {
        source: &FakeRpc::new(events),
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 200,
    }
    .sync_to_tip(Some(1))
    .await
    .expect_err("conflict");
    assert!(
        matches!(error, IngestError::Store(StoreError::Conflict { .. })),
        "{error}"
    );
    // RPC stopped before the disagreeing event.
    let checkpoint = db.store.checkpoint(RPC_STREAM).await.expect("read");
    assert!(checkpoint.is_none_or(|checkpoint| checkpoint.cursor < position));
    db.drop().await;
}

#[tokio::test]
async fn the_hand_off_starts_inside_retention_or_refuses() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };

    // Nothing seeded, nothing followed.
    let error = rpc_start(&db.store, range(1), 100)
        .await
        .expect_err("not seeded");
    assert!(matches!(error, HandoffError::NotSeeded), "{error}");

    db.store
        .write_batch(
            SEED_STREAM,
            &[],
            Checkpoint {
                cursor: end_of_ledger(5_000),
                latest_ledger: 5_000,
            },
        )
        .await
        .expect("mark seed");
    // Start `overlap` ledgers back, but never before what RPC retains.
    assert_eq!(
        rpc_start(&db.store, range(1), 100).await.expect("start"),
        Some(4_900)
    );
    assert_eq!(
        rpc_start(&db.store, range(4_950), 100)
            .await
            .expect("start"),
        Some(4_950)
    );
    // RPC retains from the ledger right after the seed: still no hole.
    assert_eq!(
        rpc_start(&db.store, range(5_001), 100)
            .await
            .expect("start"),
        Some(5_001)
    );
    // A ledger between the seed and RPC's oldest would be lost.
    let error = rpc_start(&db.store, range(5_002), 100)
        .await
        .expect_err("gap");
    assert!(
        matches!(
            error,
            HandoffError::Gap {
                stored: 5_000,
                oldest: 5_002
            }
        ),
        "{error}"
    );

    // Once following, resume from the checkpoint, unless RPC dropped past it.
    db.store
        .write_batch(
            RPC_STREAM,
            &[],
            Checkpoint {
                cursor: end_of_ledger(6_000),
                latest_ledger: 6_000,
            },
        )
        .await
        .expect("follow");
    assert_eq!(
        rpc_start(&db.store, range(6_001), 100)
            .await
            .expect("resume"),
        None
    );
    let error = rpc_start(&db.store, range(6_002), 100)
        .await
        .expect_err("gap");
    assert!(
        matches!(
            error,
            HandoffError::Gap {
                stored: 6_000,
                oldest: 6_002
            }
        ),
        "{error}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_listing_with_another_contracts_events_is_refused() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // The allowlist's records served when the pool's are asked for.
    let (server, _) = expert(vec![(POOL.to_owned(), listing("asp_CBRZ7RD5.json"))], None).await;
    let error = seed(&client(&server), &db.store, &contracts(), 10)
        .await
        .expect_err("refused");
    assert!(matches!(error, SeedError::WrongContract { .. }), "{error}");
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}

#[tokio::test]
async fn a_listing_out_of_order_is_refused() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let mut pool = listing("pool_CDJELV6H.json");
    pool.swap(0, 1);
    let (server, _) = expert(vec![(POOL.to_owned(), pool)], None).await;
    let error = seed(&client(&server), &db.store, &contracts(), 200)
        .await
        .expect_err("refused");
    assert!(matches!(error, SeedError::OutOfOrder { .. }), "{error}");
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}
