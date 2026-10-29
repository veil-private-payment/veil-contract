//! The database layer and the ingestion loop against a real Postgres.
//!
//! Every test runs on its own freshly created database (see `common`), and the
//! events are the deployment's real testnet history. Skipped locally when
//! `VEIL_TEST_DATABASE_URL` is unset, required in CI.

mod common;

use common::{
    ALLOWLIST, FakeRpc, POOL, ScriptedRpc, TestDb, contracts, end_of_ledger, history, page,
};
use stellar_xdr::curr::{ScVal, ScVec};
use veil_indexer::event::Decoded;
use veil_indexer::ingest::{Follower, IngestError, RPC_STREAM};
use veil_indexer::rpc::Start;
use veil_indexer::source::{ContractEvent, Source};
use veil_indexer::store::{Checkpoint, StoreError, StoredEvent};

/// The live pool's history: 20 transact commitments, 7 legacy deposits, 20
/// nullifiers and settlements, two wasm upgrades. The allowlist: 5 additions.
const POOL_EVENTS: i64 = 69;
const ALLOWLIST_EVENTS: i64 = 5;

fn stored(event: ContractEvent) -> StoredEvent {
    let decoded = event.raw.decode().expect("fixture event decodes");
    StoredEvent { event, decoded }
}

fn first_ledger() -> u32 {
    history()
        .first()
        .expect("history is not empty")
        .position
        .ledger()
}

async fn sync(
    db: &TestDb,
    rpc: &FakeRpc,
    page_size: u32,
) -> Result<veil_indexer::ingest::SyncReport, IngestError> {
    let contracts = contracts();
    Follower {
        source: rpc,
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size,
    }
    .sync_to_tip(Some(first_ledger()))
    .await
}

async fn assert_full_history(db: &TestDb) {
    assert_eq!(
        db.count("contract_events").await,
        POOL_EVENTS + ALLOWLIST_EVENTS
    );
    assert_eq!(db.count("pool_commitments").await, 27);
    assert_eq!(db.count("pool_legacy_deposits").await, 7);
    assert_eq!(db.count("pool_nullifiers").await, 20);
    assert_eq!(db.count("pool_settlements").await, 20);
    assert_eq!(db.count("pool_exits").await, 0);
    assert_eq!(db.count("pool_public_keys").await, 0);
    assert_eq!(db.count("allowlist_events").await, ALLOWLIST_EVENTS);
}

#[tokio::test]
async fn migrations_apply_to_an_empty_database_even_when_instances_race() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let others = [db.second_store().await, db.second_store().await];
    let (first, second, third) =
        tokio::join!(db.store.migrate(), others[0].migrate(), others[1].migrate());
    first.expect("first instance migrates");
    second.expect("second instance migrates");
    third.expect("third instance migrates");

    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(db.store.pool())
        .await
        .expect("read migrations");
    // Each migration file applied exactly once, however many instances raced.
    let files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .expect("migrations directory")
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
        })
        .count();
    assert_eq!(applied, i64::try_from(files).expect("fits"));
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}

#[tokio::test]
async fn the_full_history_lands_once_and_a_second_sync_adds_nothing() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let rpc = FakeRpc::new(history());

    let report = sync(&db, &rpc, 7).await.expect("sync");
    assert_eq!(report.inserted, 74);
    assert_eq!(report.already_stored, 0);
    // The two wasm upgrades the host reported under the pool's ID.
    assert_eq!(report.ignored, 2);
    assert_full_history(&db).await;

    let checkpoint = db
        .store
        .checkpoint(RPC_STREAM)
        .await
        .expect("read")
        .expect("set");
    assert_eq!(checkpoint.cursor.ledger(), checkpoint.latest_ledger);

    let again = sync(&db, &rpc, 7).await.expect("second sync");
    assert_eq!(again.inserted, 0);
    assert_eq!(again.pages, 1);
    assert_full_history(&db).await;
    db.drop().await;
}

#[tokio::test]
async fn writing_the_same_batch_twice_stores_each_row_once() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let batch: Vec<StoredEvent> = history().into_iter().map(stored).collect();
    let checkpoint = Checkpoint {
        cursor: batch.last().expect("not empty").event.position,
        latest_ledger: 0,
    };

    let first = db
        .store
        .write_batch("test", &batch, checkpoint)
        .await
        .expect("first write");
    let dump = db.dump().await;
    let second = db
        .store
        .write_batch("test", &batch, checkpoint)
        .await
        .expect("second write");

    assert_eq!(first.inserted, batch.len());
    assert_eq!(second.inserted, 0);
    assert_eq!(second.already_stored, batch.len());
    assert_eq!(db.dump().await, dump);
    assert_full_history(&db).await;
    db.drop().await;
}

#[tokio::test]
async fn an_interrupted_sync_resumes_after_the_last_committed_page() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };

    let crashing = FakeRpc::new(history()).failing_after(3);
    let error = sync(&db, &crashing, 5).await.expect_err("the source fails");
    assert!(matches!(error, IngestError::Rpc(_)), "{error}");
    let committed = db
        .store
        .checkpoint(RPC_STREAM)
        .await
        .expect("read")
        .expect("three pages committed");
    let before_cursor = history()
        .iter()
        .filter(|event| event.position <= committed.cursor)
        .count();
    assert!(before_cursor > 0);
    assert_eq!(
        db.count("contract_events").await,
        i64::try_from(before_cursor).expect("fits")
    );

    let rpc = FakeRpc::new(history());
    sync(&db, &rpc, 5).await.expect("resumed sync");
    assert_eq!(
        rpc.requests().first(),
        Some(&Start::After(committed.cursor))
    );
    assert_full_history(&db).await;

    // The result is row for row what a single uninterrupted run produces.
    let Some(clean) = TestDb::migrated().await else {
        return;
    };
    sync(&clean, &FakeRpc::new(history()), 5)
        .await
        .expect("clean sync");
    assert_eq!(db.dump().await, clean.dump().await);
    db.drop().await;
    clean.drop().await;
}

#[tokio::test]
async fn page_size_does_not_change_what_is_stored() {
    let Some(small) = TestDb::migrated().await else {
        return;
    };
    let Some(large) = TestDb::migrated().await else {
        return;
    };
    sync(&small, &FakeRpc::new(history()), 1)
        .await
        .expect("page size 1");
    sync(&large, &FakeRpc::new(history()), 10_000)
        .await
        .expect("page size 10000");
    assert_eq!(small.dump().await, large.dump().await);
    small.drop().await;
    large.drop().await;
}

#[tokio::test]
async fn a_seeded_event_read_again_from_rpc_is_the_same_row() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let event = history().remove(0);
    let checkpoint = Checkpoint {
        cursor: event.position,
        latest_ledger: 0,
    };
    let seeded = StoredEvent {
        event: ContractEvent {
            source: Source::StellarExpert,
            ..event.clone()
        },
        ..stored(event.clone())
    };
    let from_rpc = StoredEvent {
        event: ContractEvent {
            source: Source::Rpc,
            ..event
        },
        ..seeded.clone()
    };

    db.store
        .write_batch("seed", &[seeded], checkpoint)
        .await
        .expect("seed");
    let outcome = db
        .store
        .write_batch(RPC_STREAM, &[from_rpc], checkpoint)
        .await
        .expect("rpc");
    assert_eq!(outcome.already_stored, 1);
    let source: String = sqlx::query_scalar("SELECT source FROM contract_events")
        .fetch_one(db.store.pool())
        .await
        .expect("read source");
    assert_eq!(source, "stellar_expert");
    db.drop().await;
}

#[tokio::test]
async fn a_different_event_at_a_stored_position_is_refused_and_the_batch_rolls_back() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let mut events = history();
    let original = events.remove(0);
    let next = events.remove(0);
    db.store
        .write_batch(
            "test",
            &[stored(original.clone())],
            Checkpoint {
                cursor: original.position,
                latest_ledger: 0,
            },
        )
        .await
        .expect("first write");

    let mut tampered = original.clone();
    tampered.raw.topics.push(ScVal::Vec(Some(ScVec::default())));
    let batch = [
        stored(next.clone()),
        StoredEvent {
            event: tampered,
            decoded: Decoded::Ignored {
                name: "tampered".to_owned(),
            },
        },
    ];
    let error = db
        .store
        .write_batch(
            "test",
            &batch,
            Checkpoint {
                cursor: next.position,
                latest_ledger: 0,
            },
        )
        .await
        .expect_err("conflict");
    assert!(matches!(error, StoreError::Conflict { .. }), "{error}");

    assert_eq!(
        db.count("contract_events").await,
        1,
        "the batch was not rolled back"
    );
    let checkpoint = db
        .store
        .checkpoint("test")
        .await
        .expect("read")
        .expect("set");
    assert_eq!(checkpoint.cursor, original.position);
    db.drop().await;
}

#[tokio::test]
async fn a_tree_index_claimed_by_another_event_is_refused() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let commitment = history()
        .into_iter()
        .find(|event| matches!(event.raw.decode(), Ok(Decoded::Event(veil)) if veil.name() == "new_commitment_event"))
        .expect("a commitment");
    let position = commitment.position;
    db.store
        .write_batch(
            "test",
            &[stored(commitment.clone())],
            Checkpoint {
                cursor: position,
                latest_ledger: 0,
            },
        )
        .await
        .expect("first write");

    // The same commitment event again, at a later position.
    let moved = veil_indexer::position::EventPosition::new(
        position.toid(),
        position.event_index().saturating_add(100),
    );
    let replay = ContractEvent {
        position: moved,
        ..commitment
    };
    let error = db
        .store
        .write_batch(
            "test",
            &[stored(replay)],
            Checkpoint {
                cursor: moved,
                latest_ledger: 0,
            },
        )
        .await
        .expect_err("unique violation");
    assert!(matches!(error, StoreError::Database(_)), "{error}");
    assert_eq!(db.count("pool_commitments").await, 1);
    db.drop().await;
}

#[tokio::test]
async fn the_checkpoint_never_moves_back() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let ahead = Checkpoint {
        cursor: end_of_ledger(200),
        latest_ledger: 200,
    };
    let behind = Checkpoint {
        cursor: end_of_ledger(100),
        latest_ledger: 200,
    };
    db.store
        .write_batch("test", &[], ahead)
        .await
        .expect("advance");
    let error = db
        .store
        .write_batch("test", &[], behind)
        .await
        .expect_err("regression");
    assert!(
        matches!(error, StoreError::CheckpointRegression { .. }),
        "{error}"
    );
    assert_eq!(
        db.store.checkpoint("test").await.expect("read"),
        Some(ahead)
    );
    db.drop().await;
}

#[tokio::test]
async fn an_event_from_the_wrong_contract_stops_ingestion() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // Allowlist events that claim to come from the pool.
    let forged: Vec<ContractEvent> = history()
        .into_iter()
        .filter(|event| event.contract == ALLOWLIST)
        .map(|event| ContractEvent {
            contract: POOL.to_owned(),
            ..event
        })
        .collect();
    let error = sync(&db, &FakeRpc::new(forged), 10)
        .await
        .expect_err("refused");
    assert!(
        matches!(error, IngestError::WrongContract { .. }),
        "{error}"
    );
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}

#[tokio::test]
async fn a_stream_without_a_checkpoint_needs_a_start_ledger() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let contracts = contracts();
    let rpc = FakeRpc::new(history());
    let error = Follower {
        source: &rpc,
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 10,
    }
    .sync_to_tip(None)
    .await
    .expect_err("no start");
    assert!(matches!(error, IngestError::NoStart { .. }), "{error}");
    assert!(rpc.requests().is_empty());
    db.drop().await;
}

async fn sync_with<S: veil_indexer::rpc::EventSource>(
    db: &TestDb,
    source: &S,
    page_size: u32,
) -> Result<veil_indexer::ingest::SyncReport, IngestError> {
    let contracts = contracts();
    Follower {
        source,
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size,
    }
    .sync_to_tip(Some(1))
    .await
}

#[tokio::test]
async fn empty_ledgers_still_move_the_checkpoint_one_scan_window_at_a_time() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // No events, 20,000 ledgers, 5,000 scanned per request: four pages.
    let rpc = FakeRpc::new(Vec::new()).with_latest_ledger(20_001);
    let report = sync_with(&db, &rpc, 10).await.expect("sync");
    assert_eq!(report.pages, 4);
    assert_eq!(report.inserted, 0);
    let checkpoint = db.store.checkpoint(RPC_STREAM).await.expect("read");
    assert_eq!(checkpoint.map(|c| c.cursor), Some(end_of_ledger(20_001)));
    // Each request resumed from the previous page's cursor.
    let requests = rpc.requests();
    assert_eq!(requests[0], Start::Ledger(1));
    assert_eq!(requests[1], Start::After(end_of_ledger(5_001)));
    db.drop().await;
}

#[tokio::test]
async fn a_full_page_at_the_latest_ledger_is_not_taken_for_the_tip() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // The first ledger with several events (one transact emits a commitment,
    // a nullifier and a settlement per input).
    let events = history();
    let ledger = events
        .iter()
        .map(|event| event.position.ledger())
        .find(|ledger| {
            events
                .iter()
                .filter(|event| event.position.ledger() == *ledger)
                .count()
                >= 3
        })
        .expect("a ledger with three events");
    let in_ledger: Vec<_> = events
        .into_iter()
        .filter(|event| event.position.ledger() == ledger)
        .collect();

    // That ledger is the server's newest. The first page is full and stops
    // inside it, so the follower must ask again rather than call it the tip.
    let rpc = ScriptedRpc::new(vec![
        page(in_ledger[..2].to_vec(), in_ledger[1].position, ledger),
        page(in_ledger[2..3].to_vec(), end_of_ledger(ledger), ledger),
    ]);
    let report = sync_with(&db, &rpc, 2).await.expect("sync");
    assert_eq!(report.pages, 2);
    assert_eq!(report.inserted, 3);
    db.drop().await;
}

#[tokio::test]
async fn events_out_of_order_stop_ingestion_with_nothing_stored() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let mut events: Vec<_> = history().into_iter().take(5).collect();
    events.swap(1, 2);
    let rpc = ScriptedRpc::new(vec![page(events, end_of_ledger(5_000_000), 5_000_000)]);
    let error = sync_with(&db, &rpc, 10).await.expect_err("refused");
    assert!(matches!(error, IngestError::OutOfOrder { .. }), "{error}");
    assert_eq!(db.count("contract_events").await, 0);
    assert_eq!(db.store.checkpoint(RPC_STREAM).await.expect("read"), None);
    db.drop().await;
}

#[tokio::test]
async fn a_repeated_event_on_the_next_page_stops_ingestion() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let events: Vec<_> = history().into_iter().take(4).collect();
    // The second page starts again at the last event of the first.
    let rpc = ScriptedRpc::new(vec![
        page(events[..2].to_vec(), events[1].position, 5_000_000),
        page(events[1..].to_vec(), end_of_ledger(5_000_000), 5_000_000),
    ]);
    let error = sync_with(&db, &rpc, 2).await.expect_err("refused");
    assert!(matches!(error, IngestError::OutOfOrder { .. }), "{error}");
    // The first page committed, the second did not.
    assert_eq!(db.count("contract_events").await, 2);
    assert_eq!(
        db.store
            .checkpoint(RPC_STREAM)
            .await
            .expect("read")
            .map(|c| c.cursor),
        Some(events[1].position)
    );
    db.drop().await;
}

#[tokio::test]
async fn a_cursor_behind_the_page_it_ends_stops_ingestion() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let events: Vec<_> = history().into_iter().take(3).collect();
    let rpc = ScriptedRpc::new(vec![page(events.clone(), events[0].position, 5_000_000)]);
    let error = sync_with(&db, &rpc, 10).await.expect_err("refused");
    assert!(
        matches!(error, IngestError::CursorRegression { .. }),
        "{error}"
    );
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}

#[tokio::test]
async fn an_unknown_event_from_the_pool_stops_ingestion() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let mut events: Vec<_> = history().into_iter().take(3).collect();
    // Rename the second event, as a contract upgrade adding an event would.
    events[1].raw.topics[0] = ScVal::Symbol("brand_new_event".try_into().expect("symbol"));
    let rpc = ScriptedRpc::new(vec![page(events, end_of_ledger(5_000_000), 5_000_000)]);
    let error = sync_with(&db, &rpc, 10).await.expect_err("refused");
    assert!(matches!(error, IngestError::Decode { .. }), "{error}");
    assert_eq!(db.count("contract_events").await, 0);
    db.drop().await;
}

#[tokio::test]
async fn a_settlement_naming_another_pool_stops_ingestion() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    // The pool's history replayed as if a second pool were the one followed:
    // the contract matches, the pool named inside the settlement does not.
    let other_pool = "CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB";
    let events: Vec<ContractEvent> = history()
        .into_iter()
        .filter(|event| event.contract == POOL)
        .map(|event| ContractEvent {
            contract: other_pool.to_owned(),
            ..event
        })
        .collect();
    let contracts = veil_indexer::ingest::Contracts {
        pool: other_pool.to_owned(),
        allowlist: ALLOWLIST.to_owned(),
    };
    let rpc = FakeRpc::new(events);
    let error = Follower {
        source: &rpc,
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 200,
    }
    .sync_to_tip(Some(1))
    .await
    .expect_err("refused");
    assert!(
        matches!(error, IngestError::WrongContract { .. }),
        "{error}"
    );
    assert_eq!(db.count("pool_settlements").await, 0);
    db.drop().await;
}
