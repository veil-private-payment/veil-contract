//! Reading contract state from the ledger.
//!
//! The responses in `fixtures/rpc/get_ledger_entries_*.json` and
//! `get_health.json` were recorded from testnet RPC on 2026-10-28 at ledger
//! 5,428,145, for the deployment in `deployments/testnet.json`.

use serde_json::{Value, json};
use soroban_sdk::xdr::ScVal as SdkScVal;
use soroban_sdk::{Env, IntoVal, TryFromVal, Val};
use stellar_xdr::curr::ScVal;
use veil_indexer::chain::{ChainError, allowlist_keys, key, ledger_keys, pool_keys};
use veil_indexer::rpc::{Retry, RpcClient};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POOL: &str = "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G";
const ALLOWLIST: &str = "CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB";
const POOL_ROOT: &str = "0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd";
const ALLOWLIST_ROOT: &str = "0x04cda14213153b237712d291e1e8e74a222f4e257a6b8726ec2c167955d93738";

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/rpc/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON")
}

fn client(server: &MockServer) -> RpcClient {
    let retry = Retry {
        attempts: 1,
        first_delay: std::time::Duration::from_millis(1),
        max_delay: std::time::Duration::from_millis(1),
    };
    RpcClient::new(&server.uri(), std::time::Duration::from_secs(5), retry).expect("client")
}

async fn serve_entries(server: &MockServer, contract: &str, keys: &[ScVal], response: Value) {
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "method": "getLedgerEntries",
            "params": { "keys": ledger_keys(contract, keys).expect("keys") }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

fn sdk(env: &Env, value: impl IntoVal<Env, Val>) -> ScVal {
    let val: Val = value.into_val(env);
    // soroban-sdk re-exports the same stellar-xdr types.
    SdkScVal::try_from_val(env, &val).expect("converts")
}

#[test]
fn storage_keys_are_encoded_as_the_contract_encodes_them() {
    use pool::merkle_with_history::MerkleDataKey;
    let env = Env::default();
    assert_eq!(
        sdk(&env, MerkleDataKey::Levels),
        key("Levels", None).expect("key")
    );
    assert_eq!(
        sdk(&env, MerkleDataKey::NextIndex),
        key("NextIndex", None).expect("key")
    );
    assert_eq!(
        sdk(&env, MerkleDataKey::CurrentRootIndex),
        key("CurrentRootIndex", None).expect("key")
    );
    assert_eq!(
        sdk(&env, MerkleDataKey::Root(42)),
        key("Root", Some(ScVal::U32(42))).expect("key")
    );
    assert_eq!(pool_keys().expect("keys").len(), 93);
}

#[tokio::test]
async fn the_recorded_pool_state_is_read() {
    let server = MockServer::start().await;
    let keys = pool_keys().expect("keys");
    serve_entries(
        &server,
        POOL,
        &keys,
        fixture("get_ledger_entries_pool_CDJELV6H.json"),
    )
    .await;
    let tree = client(&server).pool_tree(POOL).await.expect("pool state");
    assert_eq!(tree.ledger, 5_428_145);
    assert_eq!(tree.levels, 16);
    assert_eq!(tree.next_index, 34);
    assert_eq!(tree.root.to_string(), POOL_ROOT);
}

#[tokio::test]
async fn the_recorded_allowlist_state_is_read() {
    let server = MockServer::start().await;
    let keys = allowlist_keys().expect("keys");
    serve_entries(
        &server,
        ALLOWLIST,
        &keys,
        fixture("get_ledger_entries_allowlist_CBRZ7RD5.json"),
    )
    .await;
    let tree = client(&server)
        .allowlist_tree(ALLOWLIST)
        .await
        .expect("state");
    assert_eq!(tree.levels, 16);
    assert_eq!(tree.next_index, 5);
    assert_eq!(tree.root.to_string(), ALLOWLIST_ROOT);
    // Never revoked, so the flag was never written.
    assert!(!tree.operator_maintained);
}

#[tokio::test]
async fn a_missing_current_root_is_an_error_not_a_default() {
    let server = MockServer::start().await;
    let mut response = fixture("get_ledger_entries_pool_CDJELV6H.json");
    // Keep Levels, NextIndex and CurrentRootIndex only.
    let entries = response["result"]["entries"]
        .as_array_mut()
        .expect("entries");
    entries.truncate(3);
    serve_entries(&server, POOL, &pool_keys().expect("keys"), response).await;
    let error = client(&server).pool_tree(POOL).await.expect_err("missing");
    assert!(matches!(error, ChainError::Missing { .. }), "{error}");
}

#[tokio::test]
async fn an_entry_of_another_contract_is_refused() {
    let server = MockServer::start().await;
    // The allowlist's entries returned for a request about the pool.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(fixture("get_ledger_entries_allowlist_CBRZ7RD5.json")),
        )
        .mount(&server)
        .await;
    let error = client(&server).pool_tree(POOL).await.expect_err("refused");
    assert!(matches!(error, ChainError::Entry(_)), "{error}");
}

#[tokio::test]
async fn the_retention_window_is_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "getHealth" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("get_health.json")))
        .mount(&server)
        .await;
    let range = client(&server).ledger_range().await.expect("range");
    assert_eq!(range.latest_ledger, 5_428_145);
    assert_eq!(range.oldest_ledger, 5_307_186);
}
