//! `verify` against the real testnet state, and against an index that is
//! wrong in each way it could be.
//!
//! The on-chain values are the ones recorded at ledger 5,428,145
//! (`fixtures/rpc/get_ledger_entries_*.json`): the pool's tree has 34
//! positions and root `0x2658…80fd`, the allowlist 5 members and root
//! `0x04cd…3738`.

mod common;

use common::{ALLOWLIST, FakeRpc, POOL, TestDb, contracts, history};
use serde_json::{Value, json};
use veil_indexer::chain::{AllowlistTree, PoolTree};
use veil_indexer::event::U256Be;
use veil_indexer::ingest::{Follower, RPC_STREAM};
use veil_indexer::rpc::{Retry, RpcClient};
use veil_indexer::seed::end_of_ledger;
use veil_indexer::source::ExpertEventRecord;
use veil_indexer::store::{Checkpoint, StoredEvent};
use veil_indexer::verify::{Check, VerifyError, check_allowlist, check_pool, verify};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const STATE_LEDGER: u32 = 5_428_145;

fn u256(hex: &str) -> U256Be {
    let mut out = [0u8; 32];
    for (slot, pair) in out
        .iter_mut()
        .zip(hex.trim_start_matches("0x").as_bytes().chunks(2))
    {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).expect("ascii"), 16).expect("hex");
    }
    U256Be(out)
}

fn pool_state() -> PoolTree {
    PoolTree {
        ledger: STATE_LEDGER,
        levels: 16,
        next_index: 34,
        root: u256("0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd"),
    }
}

fn allowlist_state() -> AllowlistTree {
    AllowlistTree {
        ledger: STATE_LEDGER,
        levels: 16,
        next_index: 5,
        root: u256("0x04cda14213153b237712d291e1e8e74a222f4e257a6b8726ec2c167955d93738"),
        operator_maintained: false,
    }
}

async fn indexed() -> Option<TestDb> {
    let db = TestDb::migrated().await?;
    let contracts = contracts();
    Follower {
        source: &FakeRpc::new(history()),
        store: &db.store,
        contracts: &contracts,
        stream: RPC_STREAM,
        page_size: 200,
    }
    .sync_to_tip(Some(1))
    .await
    .expect("sync");
    Some(db)
}

fn failed(checks: &[Check]) -> Vec<&str> {
    checks
        .iter()
        .filter(|check| !check.ok())
        .map(|check| check.name.as_str())
        .collect()
}

#[tokio::test]
async fn the_index_of_the_testnet_history_matches_the_recorded_chain_state() {
    let Some(db) = indexed().await else { return };
    let pool = check_pool(&db.store, POOL, &pool_state())
        .await
        .expect("pool");
    let allowlist = check_allowlist(&db.store, ALLOWLIST, &allowlist_state())
        .await
        .expect("allowlist");
    assert!(failed(&pool).is_empty(), "{pool:#?}");
    assert!(failed(&allowlist).is_empty(), "{allowlist:#?}");
    assert_eq!(pool.len(), 3);
    assert_eq!(allowlist.len(), 5);
    db.drop().await;
}

#[tokio::test]
async fn a_missing_commitment_fails_the_pool_checks() {
    let Some(db) = indexed().await else { return };
    sqlx::query("DELETE FROM pool_commitments WHERE pool = $1 AND leaf_index = 12")
        .bind(POOL)
        .execute(db.store.pool())
        .await
        .expect("delete");
    let checks = check_pool(&db.store, POOL, &pool_state())
        .await
        .expect("pool");
    assert_eq!(
        failed(&checks),
        ["pool: tree positions 0..n all filled", "pool: root"]
    );
    db.drop().await;
}

#[tokio::test]
async fn a_wrong_commitment_value_fails_only_the_root() {
    let Some(db) = indexed().await else { return };
    sqlx::query("UPDATE pool_commitments SET commitment = $1 WHERE pool = $2 AND leaf_index = 33")
        .bind(vec![1u8; 32])
        .bind(POOL)
        .execute(db.store.pool())
        .await
        .expect("update");
    let checks = check_pool(&db.store, POOL, &pool_state())
        .await
        .expect("pool");
    assert_eq!(failed(&checks), ["pool: root"]);
    db.drop().await;
}

#[tokio::test]
async fn a_missing_final_pair_fails_the_size_and_root() {
    let Some(db) = indexed().await else { return };
    sqlx::query("DELETE FROM pool_commitments WHERE pool = $1 AND leaf_index >= 32")
        .bind(POOL)
        .execute(db.store.pool())
        .await
        .expect("delete");
    let checks = check_pool(&db.store, POOL, &pool_state())
        .await
        .expect("pool");
    assert_eq!(failed(&checks), ["pool: next index", "pool: root"]);
    db.drop().await;
}

#[tokio::test]
async fn events_after_the_state_ledger_are_left_out() {
    let Some(db) = indexed().await else { return };
    // The ledger of the pair at 30 and 31: the pair at 32 and 33 came later.
    let ledger = history()
        .into_iter()
        .filter(|event| event.contract == POOL)
        .map(|event| event.position.ledger())
        .filter(|ledger| *ledger < 4_676_553)
        .max()
        .expect("earlier ledger");
    let earlier = PoolTree {
        ledger,
        next_index: 32,
        ..pool_state()
    };
    let checks = check_pool(&db.store, POOL, &earlier).await.expect("pool");
    // Same size as the chain had then. Its root then is unknown here, so
    // only that check fails.
    assert_eq!(failed(&checks), ["pool: root"]);
    db.drop().await;
}

#[tokio::test]
async fn a_tampered_allowlist_root_is_caught() {
    let Some(db) = indexed().await else { return };
    sqlx::query("UPDATE allowlist_events SET root = $1 WHERE leaf_index = 2")
        .bind(vec![2u8; 32])
        .execute(db.store.pool())
        .await
        .expect("update");
    let checks = check_allowlist(&db.store, ALLOWLIST, &allowlist_state())
        .await
        .expect("allowlist");
    assert_eq!(
        failed(&checks),
        ["allowlist: every root emitted before a revocation recomputes"]
    );
    db.drop().await;
}

#[tokio::test]
async fn an_operator_maintained_allowlist_is_checked_against_its_published_root() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let revocation = "CBYUTPDEXTMK6DD53FNY4KHPGY3F6MZFVM27LSRRZJUKBADAZEI6FGBL";
    let path = format!(
        "{}/tests/fixtures/stellar_expert/asp_revocation_CBYUTPDE.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let records: Vec<ExpertEventRecord> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    let batch: Vec<StoredEvent> = records
        .into_iter()
        .map(|record| {
            let event = record.into_event().expect("parses");
            let decoded = event.raw.decode().expect("decodes");
            StoredEvent { event, decoded }
        })
        .collect();
    let last = batch.last().expect("events").event.position;
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
    let published: Vec<u8> = sqlx::query_scalar(
        "SELECT root FROM allowlist_events ORDER BY toid DESC, event_index DESC LIMIT 1",
    )
    .fetch_one(db.store.pool())
    .await
    .expect("root");

    let state = AllowlistTree {
        ledger: u32::MAX >> 1,
        levels: 16,
        next_index: 1,
        root: U256Be(published.try_into().expect("32 bytes")),
        operator_maintained: true,
    };
    let checks = check_allowlist(&db.store, revocation, &state)
        .await
        .expect("checks");
    assert!(failed(&checks).is_empty(), "{checks:#?}");
    // No recomputation once the operator publishes the roots.
    assert!(
        checks
            .iter()
            .all(|check| check.name != "allowlist: recomputed root")
    );

    // The chain still saying "not operator-maintained" would be a mismatch.
    let checks = check_allowlist(
        &db.store,
        revocation,
        &AllowlistTree {
            operator_maintained: false,
            ..state
        },
    )
    .await
    .expect("checks");
    assert_eq!(failed(&checks), ["allowlist: operator-maintained"]);
    db.drop().await;
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/rpc/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON")
}

async fn chain_server() -> MockServer {
    let server = MockServer::start().await;
    for (contract, keys, name) in [
        (
            POOL,
            veil_indexer::chain::pool_keys().expect("keys"),
            "get_ledger_entries_pool_CDJELV6H.json",
        ),
        (
            ALLOWLIST,
            veil_indexer::chain::allowlist_keys().expect("keys"),
            "get_ledger_entries_allowlist_CBRZ7RD5.json",
        ),
    ] {
        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "method": "getLedgerEntries",
                "params": { "keys": veil_indexer::chain::ledger_keys(contract, &keys).expect("keys") }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(name)))
            .mount(&server)
            .await;
    }
    server
}

fn rpc(server: &MockServer) -> RpcClient {
    let retry = Retry {
        attempts: 1,
        first_delay: std::time::Duration::from_millis(1),
        max_delay: std::time::Duration::from_millis(1),
    };
    RpcClient::new(&server.uri(), std::time::Duration::from_secs(5), retry).expect("client")
}

#[tokio::test]
async fn verify_end_to_end_reads_the_chain_and_passes() {
    let Some(db) = indexed().await else { return };
    // The index has reached the state's ledger.
    db.store
        .write_batch(
            RPC_STREAM,
            &[],
            Checkpoint {
                cursor: end_of_ledger(STATE_LEDGER),
                latest_ledger: STATE_LEDGER,
            },
        )
        .await
        .expect("advance");
    let server = chain_server().await;
    let report = verify(
        &rpc(&server),
        &db.store,
        &contracts(),
        std::time::Duration::ZERO,
    )
    .await
    .expect("verify");
    assert!(report.ok(), "{report:#?}");
    assert_eq!(report.pool_ledger, STATE_LEDGER);
    assert_eq!(report.checks.len(), 8);
    db.drop().await;
}

#[tokio::test]
async fn verify_refuses_to_compare_an_index_behind_the_state() {
    let Some(db) = indexed().await else { return };
    let server = chain_server().await;
    let error = verify(
        &rpc(&server),
        &db.store,
        &contracts(),
        std::time::Duration::ZERO,
    )
    .await
    .expect_err("behind");
    assert!(
        matches!(
            error,
            VerifyError::Behind {
                state: STATE_LEDGER,
                ..
            }
        ),
        "{error}"
    );
    db.drop().await;
}
