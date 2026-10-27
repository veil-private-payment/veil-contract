//! What the database holds, value by value.
//!
//! `store_and_ingest.rs` checks how many rows land and when writes are
//! refused. This file checks that each row carries exactly the values the
//! contract emitted: every field of the real testnet history, the raw XDR
//! decoding back to the same event, and the event kinds the deployment has
//! never emitted (`Exit`, `public_key_event`, a withdrawal settlement), which
//! are published here through the contracts' own event structs.

mod common;

use common::{FakeRpc, POOL, TestDb, contracts, history};
use soroban_sdk::testutils::{Address as _, Events as _};
use soroban_sdk::xdr::{self, ContractEventBody};
use soroban_sdk::{Address, Bytes, Env, U256, contract, contractimpl};
use sqlx::Row;
use veil_indexer::event::{Decoded, RawEvent, U256Be, VeilEvent};
use veil_indexer::ingest::{Follower, RPC_STREAM};
use veil_indexer::position::EventPosition;
use veil_indexer::source::{ContractEvent, ExpertEventRecord, Source};
use veil_indexer::store::{Checkpoint, StoredEvent};

fn stored(event: ContractEvent) -> StoredEvent {
    let decoded = event.raw.decode().expect("event decodes");
    StoredEvent { event, decoded }
}

fn veil(event: &ContractEvent) -> Option<VeilEvent> {
    match event.raw.decode().expect("event decodes") {
        Decoded::Event(veil) => Some(veil),
        Decoded::Ignored { .. } => None,
    }
}

fn key(position: EventPosition) -> (i64, i32) {
    (
        i64::try_from(position.toid()).expect("toid fits"),
        i32::try_from(position.event_index()).expect("index fits"),
    )
}

async fn sync_history(db: &TestDb) {
    let contracts = contracts();
    let first = history().first().expect("history").position.ledger();
    Follower {
        source: &FakeRpc::new(history()),
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 50,
    }
    .sync_to_tip(Some(first))
    .await
    .expect("sync");
}

async fn row(
    db: &TestDb,
    table: &str,
    columns: &str,
    position: EventPosition,
) -> sqlx::postgres::PgRow {
    let (toid, index) = key(position);
    sqlx::query(&format!(
        "SELECT {columns} FROM {table} WHERE toid = $1 AND event_index = $2"
    ))
    .bind(toid)
    .bind(index)
    .fetch_one(db.store.pool())
    .await
    .unwrap_or_else(|err| panic!("{table} row at {position}: {err}"))
}

fn bytes(row: &sqlx::postgres::PgRow, column: &str) -> Vec<u8> {
    row.try_get(column).expect(column)
}

#[tokio::test]
async fn every_stored_field_matches_the_testnet_history() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    sync_history(&db).await;

    let mut checked = 0usize;
    for event in history() {
        let position = event.position;
        let ledger = i32::try_from(position.ledger()).expect("ledger fits");
        let Some(decoded) = veil(&event) else {
            continue;
        };
        match decoded {
            VeilEvent::NewCommitment {
                commitment,
                index,
                encrypted_output,
            } => {
                let row = row(
                    &db,
                    "pool_commitments",
                    "pool, leaf_index, commitment, encrypted_output, origin, ledger",
                    position,
                )
                .await;
                assert_eq!(row.get::<String, _>("pool"), POOL);
                assert_eq!(row.get::<i64, _>("leaf_index"), i64::from(index));
                assert_eq!(bytes(&row, "commitment"), commitment.as_bytes());
                assert_eq!(
                    row.get::<Option<Vec<u8>>, _>("encrypted_output"),
                    Some(encrypted_output)
                );
                assert_eq!(row.get::<String, _>("origin"), "transact");
                assert_eq!(row.get::<i32, _>("ledger"), ledger);
            }
            VeilEvent::LegacyDeposit(deposit) => {
                let commitment = row(
                    &db,
                    "pool_commitments",
                    "leaf_index, commitment, encrypted_output, origin",
                    position,
                )
                .await;
                assert_eq!(
                    commitment.get::<i64, _>("leaf_index"),
                    i64::from(deposit.index)
                );
                assert_eq!(
                    bytes(&commitment, "commitment"),
                    deposit.commitment.as_bytes()
                );
                assert_eq!(
                    commitment.get::<Option<Vec<u8>>, _>("encrypted_output"),
                    None
                );
                assert_eq!(commitment.get::<String, _>("origin"), "legacy_deposit");
                let row = row(
                    &db,
                    "pool_legacy_deposits",
                    "pool, leaf_index, amount_bucket::text AS amount_bucket, asset",
                    position,
                )
                .await;
                assert_eq!(row.get::<String, _>("pool"), deposit.pool);
                assert_eq!(row.get::<i64, _>("leaf_index"), i64::from(deposit.index));
                assert_eq!(
                    row.get::<String, _>("amount_bucket"),
                    deposit.amount_bucket.to_string()
                );
                assert_eq!(row.get::<String, _>("asset"), deposit.asset);
            }
            VeilEvent::NewNullifier { nullifier } => {
                let row = row(&db, "pool_nullifiers", "pool, nullifier", position).await;
                assert_eq!(row.get::<String, _>("pool"), POOL);
                assert_eq!(bytes(&row, "nullifier"), nullifier.as_bytes());
            }
            VeilEvent::Settlement(settlement) => {
                let row = row(
                    &db,
                    "pool_settlements",
                    "pool, nullifier, output_commitment0, output_commitment1, output_index0, \
                     output_index1, amount_bucket::text AS amount_bucket, public_amount, recipient, asset",
                    position,
                )
                .await;
                assert_eq!(row.get::<String, _>("pool"), settlement.pool);
                assert_eq!(bytes(&row, "nullifier"), settlement.nullifier.as_bytes());
                assert_eq!(
                    bytes(&row, "output_commitment0"),
                    settlement.output_commitment0.as_bytes()
                );
                assert_eq!(
                    bytes(&row, "output_commitment1"),
                    settlement.output_commitment1.as_bytes()
                );
                assert_eq!(
                    row.get::<i64, _>("output_index0"),
                    i64::from(settlement.output_index0)
                );
                assert_eq!(
                    row.get::<i64, _>("output_index1"),
                    i64::from(settlement.output_index1)
                );
                assert_eq!(
                    row.get::<String, _>("amount_bucket"),
                    settlement.amount_bucket.to_string()
                );
                assert_eq!(
                    bytes(&row, "public_amount"),
                    settlement.public_amount.as_bytes()
                );
                assert_eq!(
                    row.get::<Option<String>, _>("recipient"),
                    settlement.recipient
                );
                assert_eq!(row.get::<String, _>("asset"), settlement.asset);
            }
            VeilEvent::LeafAdded { leaf, index, root } => {
                let row = row(
                    &db,
                    "allowlist_events",
                    "kind, leaf, leaf_index, root",
                    position,
                )
                .await;
                assert_eq!(row.get::<String, _>("kind"), "added");
                assert_eq!(bytes(&row, "leaf"), leaf.as_bytes());
                assert_eq!(
                    row.get::<Option<i64>, _>("leaf_index"),
                    Some(i64::try_from(index).expect("fits"))
                );
                assert_eq!(bytes(&row, "root"), root.as_bytes());
            }
            other => panic!("the testnet history has no {}", other.name()),
        }
        checked = checked.saturating_add(1);
    }
    // 74 events, of which the two wasm upgrades have no typed row.
    assert_eq!(checked, 72);
    db.drop().await;
}

#[tokio::test]
async fn the_raw_rows_decode_back_to_the_same_events() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    sync_history(&db).await;

    let rows = sqlx::query(
        "SELECT toid, event_index, contract, name, topics_xdr, data_xdr \
         FROM contract_events ORDER BY toid, event_index",
    )
    .fetch_all(db.store.pool())
    .await
    .expect("read raw events");
    let original = history();
    assert_eq!(rows.len(), original.len());

    for (row, event) in rows.iter().zip(&original) {
        let position = EventPosition::new(
            u64::try_from(row.get::<i64, _>("toid")).expect("toid"),
            u32::try_from(row.get::<i32, _>("event_index")).expect("index"),
        );
        assert_eq!(position, event.position);
        assert_eq!(row.get::<String, _>("contract"), event.contract);
        let topics: Vec<String> = row.get("topics_xdr");
        let raw = RawEvent::from_base64(&topics, &row.get::<String, _>("data_xdr"))
            .expect("stored XDR parses");
        assert_eq!(raw, event.raw, "raw event at {position}");
        let name = match raw.decode().expect("decodes") {
            Decoded::Event(veil) => veil.name().to_owned(),
            Decoded::Ignored { name } => name,
        };
        assert_eq!(row.get::<String, _>("name"), name);
    }
    db.drop().await;
}

#[tokio::test]
async fn a_revocation_is_stored_with_no_index() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let path = format!(
        "{}/tests/fixtures/stellar_expert/asp_revocation_CBYUTPDE.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let records: Vec<ExpertEventRecord> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    let batch: Vec<StoredEvent> = records
        .into_iter()
        .map(|record| stored(record.into_event().expect("parses")))
        .collect();
    let last = batch.last().expect("not empty").event.position;
    db.store
        .write_batch(
            "seed",
            &batch,
            Checkpoint {
                cursor: last,
                latest_ledger: 0,
            },
        )
        .await
        .expect("write");

    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT kind, leaf_index FROM allowlist_events ORDER BY toid, event_index")
            .fetch_all(db.store.pool())
            .await
            .expect("read");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], ("added".to_owned(), Some(0)));
    assert_eq!(rows[1].0, "revoked");
    assert_eq!(rows[1].1, None);
    assert_eq!(rows[2].0, "added");
    // The revocation's root is the one the operator published, kept as is.
    let Some(VeilEvent::LeafRevoked { root, .. }) = veil(&batch[1].event) else {
        panic!("second event is a revocation");
    };
    let stored_root: Vec<u8> =
        sqlx::query_scalar("SELECT root FROM allowlist_events WHERE kind = 'revoked'")
            .fetch_one(db.store.pool())
            .await
            .expect("read root");
    assert_eq!(stored_root, root.as_bytes());
    db.drop().await;
}

#[contract]
struct Emitter;

#[contractimpl]
impl Emitter {
    pub fn noop() {}
}

/// Publishes one event through a contract's own struct and returns it as the
/// indexer would read it, at `position`.
fn emitted(
    env: &Env,
    emitter: &Address,
    position: EventPosition,
    publish: impl FnOnce(&Env),
) -> ContractEvent {
    env.as_contract(emitter, || publish(env));
    // The host keeps the events of the latest invocation only.
    let recorded = env.events().all().filter_by_contract(emitter);
    let [event] = recorded.events() else {
        panic!("expected one event, got {:?}", recorded.events());
    };
    let ContractEventBody::V0(body) = &event.body;
    ContractEvent {
        position,
        contract: xdr::ScAddress::from(emitter).to_string(),
        source: Source::Rpc,
        raw: RawEvent {
            topics: body.topics.to_vec(),
            data: body.data.clone(),
        },
    }
}

fn at(ledger: u64, index: u32) -> EventPosition {
    EventPosition::new((ledger << 32) | 1, index)
}

fn u256(env: &Env, value: u32) -> U256 {
    U256::from_u32(env, value)
}

fn be(value: u32) -> Vec<u8> {
    let mut bytes = [0u8; 32];
    bytes[28..].copy_from_slice(&value.to_be_bytes());
    U256Be(bytes).as_bytes().to_vec()
}

#[tokio::test]
async fn event_kinds_the_deployment_never_emitted_are_stored_faithfully() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let env = Env::default();
    let pool = env.register(Emitter, ());
    let allowlist = env.register(Emitter, ());
    let asset = Address::generate(&env);
    let owner = Address::generate(&env);
    let recipient = Address::generate(&env);
    let strkey = |address: &Address| xdr::ScAddress::from(address).to_string();

    let events = vec![
        emitted(&env, &pool, at(10, 0), |env| {
            pool::event::ExitEvent {
                nullifier: u256(env, 1),
                pool: pool.clone(),
                amount_bucket: i128::MAX,
                public_amount: u256(env, 2),
                recipient: recipient.clone(),
                asset: asset.clone(),
            }
            .publish(env);
        }),
        emitted(&env, &pool, at(11, 0), |env| {
            pool::event::SettlementEvent {
                nullifier: u256(env, 3),
                pool: pool.clone(),
                output_commitment0: u256(env, 4),
                output_commitment1: u256(env, 5),
                output_index0: 6,
                output_index1: 7,
                amount_bucket: i128::MIN,
                public_amount: u256(env, 8),
                recipient: Some(recipient.clone()),
                asset: asset.clone(),
            }
            .publish(env);
        }),
        emitted(&env, &pool, at(12, 0), |env| {
            pool::event::PublicKeyEvent {
                owner: owner.clone(),
                encryption_key: Bytes::from_array(env, &[1u8; 32]),
                note_key: Bytes::from_array(env, &[2u8; 32]),
            }
            .publish(env);
        }),
        emitted(&env, &pool, at(13, 0), |env| {
            pool::event::PublicKeyEvent {
                owner: owner.clone(),
                encryption_key: Bytes::from_array(env, &[3u8; 32]),
                note_key: Bytes::from_array(env, &[4u8; 32]),
            }
            .publish(env);
        }),
        emitted(&env, &pool, at(14, 0), |env| {
            pool::event::NewCommitmentEvent {
                commitment: u256(env, 9),
                index: u32::MAX,
                encrypted_output: Bytes::new(env),
            }
            .publish(env);
        }),
        emitted(&env, &allowlist, at(15, 0), |env| {
            asp_membership::event::LeafRevokedEvent {
                leaf: u256(env, 10),
                root: u256(env, 11),
            }
            .publish(env);
        }),
    ];
    let batch: Vec<StoredEvent> = events.into_iter().map(stored).collect();
    db.store
        .write_batch(
            "test",
            &batch,
            Checkpoint {
                cursor: at(15, 0),
                latest_ledger: 15,
            },
        )
        .await
        .expect("write");

    let exit = row(
        &db,
        "pool_exits",
        "pool, nullifier, amount_bucket::text AS amount_bucket, public_amount, recipient, asset",
        at(10, 0),
    )
    .await;
    assert_eq!(exit.get::<String, _>("pool"), strkey(&pool));
    assert_eq!(bytes(&exit, "nullifier"), be(1));
    assert_eq!(
        exit.get::<String, _>("amount_bucket"),
        i128::MAX.to_string()
    );
    assert_eq!(bytes(&exit, "public_amount"), be(2));
    assert_eq!(exit.get::<String, _>("recipient"), strkey(&recipient));
    assert_eq!(exit.get::<String, _>("asset"), strkey(&asset));
    // An exit adds nothing to the tree.
    assert_eq!(db.count("pool_commitments").await, 1);

    let withdrawal = row(
        &db,
        "pool_settlements",
        "amount_bucket::text AS amount_bucket, recipient, output_index0, output_index1",
        at(11, 0),
    )
    .await;
    assert_eq!(
        withdrawal.get::<String, _>("amount_bucket"),
        i128::MIN.to_string()
    );
    assert_eq!(
        withdrawal.get::<Option<String>, _>("recipient"),
        Some(strkey(&recipient))
    );
    assert_eq!(withdrawal.get::<i64, _>("output_index0"), 6);
    assert_eq!(withdrawal.get::<i64, _>("output_index1"), 7);

    // The latest registration per owner, through the index the API will use.
    let latest = sqlx::query(
        "SELECT encryption_key, note_key FROM pool_public_keys \
         WHERE pool = $1 AND owner = $2 ORDER BY toid DESC, event_index DESC LIMIT 1",
    )
    .bind(strkey(&pool))
    .bind(strkey(&owner))
    .fetch_one(db.store.pool())
    .await
    .expect("latest key");
    assert_eq!(bytes(&latest, "encryption_key"), vec![3u8; 32]);
    assert_eq!(bytes(&latest, "note_key"), vec![4u8; 32]);
    assert_eq!(db.count("pool_public_keys").await, 2);

    // An empty note is stored as empty, not as a missing one.
    let commitment = row(
        &db,
        "pool_commitments",
        "leaf_index, encrypted_output, origin",
        at(14, 0),
    )
    .await;
    assert_eq!(commitment.get::<i64, _>("leaf_index"), i64::from(u32::MAX));
    assert_eq!(
        commitment.get::<Option<Vec<u8>>, _>("encrypted_output"),
        Some(Vec::new())
    );
    assert_eq!(commitment.get::<String, _>("origin"), "transact");

    let revoked = row(
        &db,
        "allowlist_events",
        "allowlist, kind, leaf, leaf_index, root",
        at(15, 0),
    )
    .await;
    assert_eq!(revoked.get::<String, _>("allowlist"), strkey(&allowlist));
    assert_eq!(revoked.get::<String, _>("kind"), "revoked");
    assert_eq!(bytes(&revoked, "leaf"), be(10));
    assert_eq!(revoked.get::<Option<i64>, _>("leaf_index"), None);
    assert_eq!(bytes(&revoked, "root"), be(11));
    db.drop().await;
}

#[tokio::test]
async fn two_writers_with_the_same_batch_store_it_once() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let other = db.second_store().await;
    let batch: Vec<StoredEvent> = history().into_iter().map(stored).collect();
    let checkpoint = Checkpoint {
        cursor: batch.last().expect("not empty").event.position,
        latest_ledger: 0,
    };
    let (first, second) = tokio::join!(
        db.store.write_batch(RPC_STREAM, &batch, checkpoint),
        other.write_batch(RPC_STREAM, &batch, checkpoint)
    );
    let (first, second) = (first.expect("first writer"), second.expect("second writer"));
    assert_eq!(first.inserted.saturating_add(second.inserted), batch.len());
    assert_eq!(
        first.already_stored.saturating_add(second.already_stored),
        batch.len()
    );
    assert_eq!(db.count("contract_events").await, 74);
    assert_eq!(
        db.store.checkpoint(RPC_STREAM).await.expect("read"),
        Some(checkpoint)
    );
    db.drop().await;
}
