//! The read API end to end: a real Postgres holding the deployment's real
//! testnet history (as the indexer stores it), the API served over HTTP with
//! the same read-only pool and middleware it runs with, and every answer
//! checked against the on-chain state recorded at ledger 5,428,145.
//!
//! Needs `VEIL_TEST_DATABASE_URL` (see services/indexer/docker-compose.yml).
//! Skipped locally without it, required in CI.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, Connection, Executor};
use veil_api::routes::{ApiConfig, AppState, app};
use veil_indexer::event::{Decoded, U256Be, VeilEvent};
use veil_indexer::seed::end_of_ledger;
use veil_indexer::source::{ContractEvent, ExpertEventRecord};
use veil_indexer::store::{Checkpoint, Store, StoredEvent};
use veil_indexer::tree::{Field, compress};

const POOL: &str = "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G";
const ALLOWLIST: &str = "CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB";
const STATE_LEDGER: u32 = 5_428_145;
const POOL_ROOT: &str = "0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd";
const ALLOWLIST_ROOT: &str = "0x04cda14213153b237712d291e1e8e74a222f4e257a6b8726ec2c167955d93738";

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    base: String,
    /// A writable connection for arranging test data, as the indexer has.
    writer: Store,
    admin: PgConnectOptions,
    name: String,
    http: reqwest::Client,
}

impl Fixture {
    async fn get(&self, path: &str) -> (u16, Value) {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .expect("request");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn ok(&self, path: &str) -> Value {
        let (status, body) = self.get(path).await;
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    async fn drop(self) {
        self.writer.pool().close().await;
        let mut connection = self.admin.connect().await.expect("admin");
        connection
            .execute(format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name).as_str())
            .await
            .expect("drop");
    }
}

fn history(name: &str) -> Vec<ContractEvent> {
    let path = format!(
        "{}/../indexer/tests/fixtures/stellar_expert/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let records: Vec<ExpertEventRecord> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    records
        .into_iter()
        .map(|record| record.into_event().expect("parses"))
        .collect()
}

fn all_history() -> Vec<ContractEvent> {
    let mut events = history("pool_CDJELV6H.json");
    events.extend(history("asp_CBRZ7RD5.json"));
    events.sort_by_key(|event| event.position);
    events
}

async fn setup(rate_limit: Option<(u64, u32)>) -> Option<Fixture> {
    let Ok(url) = std::env::var("VEIL_TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "VEIL_TEST_DATABASE_URL must be set in CI"
        );
        eprintln!("VEIL_TEST_DATABASE_URL is not set, skipping an API test");
        return None;
    };
    let admin = PgConnectOptions::from_str(&url).expect("URL parses");
    let name = format!(
        "veil_api_test_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut connection = admin.connect().await.expect("admin");
    connection
        .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
        .await
        .expect("drop stale");
    connection
        .execute(format!("CREATE DATABASE {name}").as_str())
        .await
        .expect("create");
    connection.close().await.expect("close");

    // Fill it as the indexer would.
    let writer = Store::from_pool(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(admin.clone().database(&name))
            .await
            .expect("writer"),
    );
    writer.migrate().await.expect("migrate");
    let batch: Vec<StoredEvent> = all_history()
        .into_iter()
        .map(|event| {
            let decoded = event.raw.decode().expect("decodes");
            StoredEvent { event, decoded }
        })
        .collect();
    writer
        .write_batch(
            "seed",
            &batch,
            Checkpoint {
                cursor: end_of_ledger(STATE_LEDGER),
                latest_ledger: STATE_LEDGER,
            },
        )
        .await
        .expect("history");
    writer
        .write_batch(
            "rpc",
            &[],
            Checkpoint {
                cursor: end_of_ledger(STATE_LEDGER),
                latest_ledger: STATE_LEDGER,
            },
        )
        .await
        .expect("checkpoint");
    for (contract, kind, next_index, root) in [
        (POOL, "pool", 34i64, POOL_ROOT),
        (ALLOWLIST, "allowlist", 5, ALLOWLIST_ROOT),
    ] {
        sqlx::query(
            "INSERT INTO verified_state (contract, kind, ledger, levels, next_index, root, matches) \
             VALUES ($1, $2, $3, 16, $4, $5, true)",
        )
        .bind(contract)
        .bind(kind)
        .bind(i64::from(STATE_LEDGER))
        .bind(next_index)
        .bind(bytes(root))
        .execute(writer.pool())
        .await
        .expect("verified state");
    }

    let db = veil_api::read_only_pool(admin.clone().database(&name), 4)
        .await
        .expect("read-only pool");
    let state = AppState {
        db,
        config: Arc::new(ApiConfig {
            network_passphrase: "Test SDF Network ; September 2015".to_owned(),
            pool: POOL.to_owned(),
            allowlist: ALLOWLIST.to_owned(),
            max_staleness: Duration::from_secs(120),
            request_timeout: Duration::from_secs(10),
            rate_limit,
        }),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let service = app(state)
        .expect("app")
        .into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await });
    Some(Fixture {
        base,
        writer,
        admin,
        name,
        http: reqwest::Client::new(),
    })
}

fn bytes(hex: &str) -> Vec<u8> {
    hex.trim_start_matches("0x")
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("ascii"), 16).expect("hex"))
        .collect()
}

fn field(hex: &str) -> Field {
    let array: [u8; 32] = bytes(hex).try_into().expect("32 bytes");
    Field::from_u256(&U256Be(array)).expect("field element")
}

/// Recomputes a root from a served path, independently of the server.
fn root_of(path: &Value) -> String {
    let mut index = path["path_indices"].as_u64().expect("indices");
    let mut current = field(path["leaf"].as_str().expect("leaf"));
    for sibling in path["path_elements"].as_array().expect("elements") {
        let sibling = field(sibling.as_str().expect("hex"));
        current = if index & 1 == 1 {
            compress(sibling, current)
        } else {
            compress(current, sibling)
        };
        index >>= 1;
    }
    current.to_u256().to_string()
}

#[tokio::test]
async fn status_reports_the_contracts_and_the_indexed_ledger() {
    let Some(fx) = setup(None).await else { return };
    let body = fx.ok("/v1/status").await;
    assert_eq!(body["pool"], POOL);
    assert_eq!(body["allowlist"], ALLOWLIST);
    assert_eq!(body["indexed_ledger"], STATE_LEDGER);
    assert!(
        body["updated_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    fx.drop().await;
}

#[tokio::test]
async fn the_pool_root_computed_from_the_index_is_the_on_chain_root() {
    let Some(fx) = setup(None).await else { return };
    let body = fx.ok("/v1/pool").await;
    assert_eq!(body["levels"], 16);
    assert_eq!(body["next_index"], 34);
    assert_eq!(body["root"], POOL_ROOT);
    assert_eq!(body["on_chain"]["root"], POOL_ROOT);
    assert_eq!(body["on_chain"]["next_index"], 34);
    assert_eq!(body["on_chain"]["matches"], true);
    fx.drop().await;
}

#[tokio::test]
async fn paging_the_leaves_returns_every_position_once_in_order() {
    let Some(fx) = setup(None).await else { return };
    let mut all = Vec::new();
    let mut cursor = Some(0u64);
    let mut pages = 0;
    while let Some(at) = cursor {
        let body = fx.ok(&format!("/v1/pool/leaves?cursor={at}&limit=5")).await;
        all.extend(body["items"].as_array().expect("items").clone());
        cursor = body["next_cursor"].as_u64();
        pages += 1;
    }
    assert_eq!(pages, 7);
    let indices: Vec<u64> = all
        .iter()
        .map(|leaf| leaf["leaf_index"].as_u64().expect("index"))
        .collect();
    assert_eq!(indices, (0..34).collect::<Vec<_>>());
    // The zero leaves the closed deposit path inserted, without events.
    let zeros: Vec<u64> = all
        .iter()
        .filter(|leaf| leaf["origin"] == "legacy_deposit_zero")
        .map(|leaf| leaf["leaf_index"].as_u64().expect("index"))
        .collect();
    assert_eq!(zeros, [1, 3, 7, 9, 15, 19, 21]);
    assert!(
        all.iter()
            .filter(|leaf| leaf["origin"] == "legacy_deposit_zero")
            .all(|leaf| {
                leaf["commitment"]
                    == "0x0000000000000000000000000000000000000000000000000000000000000000"
            })
    );

    // A cursor landing on a zero leaf starts with it.
    let body = fx.ok("/v1/pool/leaves?cursor=3&limit=1").await;
    assert_eq!(body["items"][0]["leaf_index"], 3);
    assert_eq!(body["items"][0]["origin"], "legacy_deposit_zero");
    assert_eq!(body["next_cursor"], 4);
    fx.drop().await;
}

#[tokio::test]
async fn every_pool_path_leads_to_the_on_chain_root() {
    let Some(fx) = setup(None).await else { return };
    for index in 0..34 {
        let body = fx.ok(&format!("/v1/pool/leaves/{index}/path")).await;
        assert_eq!(body["path_elements"].as_array().map(Vec::len), Some(16));
        assert_eq!(body["root"], POOL_ROOT, "leaf {index}");
        assert_eq!(root_of(&body), POOL_ROOT, "leaf {index} recomputed");
    }
    let (status, body) = fx.get("/v1/pool/leaves/34/path").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
    fx.drop().await;
}

#[tokio::test]
async fn spent_nullifiers_are_reported_and_unknown_ones_are_not() {
    let Some(fx) = setup(None).await else { return };
    let (spent, position) = history("pool_CDJELV6H.json")
        .into_iter()
        .find_map(|event| match event.raw.decode() {
            Ok(Decoded::Event(VeilEvent::NewNullifier { nullifier })) => {
                Some((nullifier, event.position))
            }
            _ => None,
        })
        .expect("a nullifier");
    let body = fx.ok(&format!("/v1/pool/nullifiers/{spent}")).await;
    assert_eq!(body["spent"], true);
    assert_eq!(body["ledger"], position.ledger());
    assert_eq!(body["event"], position.to_string());

    let unknown = format!("0x{}", "11".repeat(32));
    let body = fx.ok(&format!("/v1/pool/nullifiers/{unknown}")).await;
    assert_eq!(body["spent"], false);
    assert!(body["ledger"].is_null());

    for bad in ["0x12", "zz", &format!("0x{}", "g".repeat(64))] {
        let (status, body) = fx.get(&format!("/v1/pool/nullifiers/{bad}")).await;
        assert_eq!(status, 400, "{bad}");
        assert_eq!(body["error"]["code"], "bad_request");
    }
    fx.drop().await;
}

#[tokio::test]
async fn the_note_feed_pages_every_published_note() {
    let Some(fx) = setup(None).await else { return };
    let mut notes = Vec::new();
    let mut cursor = Some(0u64);
    while let Some(at) = cursor {
        let body = fx.ok(&format!("/v1/notes?cursor={at}&limit=6")).await;
        notes.extend(body["items"].as_array().expect("items").clone());
        cursor = body["next_cursor"].as_u64();
    }
    // 20 transact outputs: 13 with the 137-byte notes on chain, 7 empty.
    assert_eq!(notes.len(), 20);
    let lengths: Vec<usize> = notes
        .iter()
        .map(|note| {
            note["encrypted_output"]
                .as_str()
                .expect("hex")
                .len()
                .saturating_sub(2)
                / 2
        })
        .collect();
    assert_eq!(lengths.iter().filter(|len| **len == 137).count(), 13);
    assert_eq!(lengths.iter().filter(|len| **len == 0).count(), 7);
    let indices: Vec<u64> = notes
        .iter()
        .map(|note| note["leaf_index"].as_u64().expect("index"))
        .collect();
    assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
    fx.drop().await;
}

#[tokio::test]
async fn public_keys_return_the_latest_registration() {
    let Some(fx) = setup(None).await else { return };
    let owner = "GCMNXF35HYEY5CEVEYCE57DTD4MGFIKQP5II54YK5SMRSXLFFZC7HRXN";
    let (status, _) = fx.get(&format!("/v1/public-keys/{owner}")).await;
    assert_eq!(status, 404);

    // Two registrations by the same owner, the second later.
    for (ledger, key) in [(5_000_000i64, 1u8), (5_000_001, 2)] {
        let toid = ledger << 32 | 1;
        sqlx::query(
            "INSERT INTO contract_events (toid, event_index, ledger, contract, source, name, topics_xdr, data_xdr) \
             VALUES ($1, 0, $2, $3, 'rpc', 'public_key_event', '{}', '')",
        )
        .bind(toid)
        .bind(ledger)
        .bind(POOL)
        .execute(fx.writer.pool())
        .await
        .expect("raw event");
        sqlx::query(
            "INSERT INTO pool_public_keys (toid, event_index, pool, owner, encryption_key, note_key, ledger) \
             VALUES ($1, 0, $2, $3, $4, $5, $6)",
        )
        .bind(toid)
        .bind(POOL)
        .bind(owner)
        .bind(vec![key; 32])
        .bind(vec![key.saturating_add(10); 32])
        .bind(ledger)
        .execute(fx.writer.pool())
        .await
        .expect("key");
    }
    let body = fx.ok(&format!("/v1/public-keys/{owner}")).await;
    assert_eq!(body["encryption_key"], format!("0x{}", "02".repeat(32)));
    assert_eq!(body["note_key"], format!("0x{}", "0c".repeat(32)));
    assert_eq!(body["ledger"], 5_000_001);
    assert_eq!(body["registrations"], 2);

    let (status, body) = fx.get("/v1/public-keys/not-an-address").await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "bad_request");
    fx.drop().await;
}

#[tokio::test]
async fn the_allowlist_and_its_paths_match_the_on_chain_root() {
    let Some(fx) = setup(None).await else { return };
    let body = fx.ok("/v1/allowlist").await;
    assert_eq!(body["additions"], 5);
    assert_eq!(body["revocations"], 0);
    assert_eq!(body["operator_maintained"], false);
    assert_eq!(body["root"], ALLOWLIST_ROOT);
    assert_eq!(body["on_chain"]["root"], ALLOWLIST_ROOT);

    let first = fx.ok("/v1/allowlist/entries?limit=3").await;
    let second = fx
        .ok(&format!(
            "/v1/allowlist/entries?limit=3&cursor={}",
            first["next_cursor"].as_str().expect("cursor")
        ))
        .await;
    assert!(second["next_cursor"].is_null());
    let entries: Vec<Value> = first["items"]
        .as_array()
        .into_iter()
        .chain(second["items"].as_array())
        .flatten()
        .cloned()
        .collect();
    assert_eq!(entries.len(), 5);
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(entry["leaf_index"], index);
        let path = fx
            .ok(&format!(
                "/v1/allowlist/leaves/{}/path",
                entry["leaf"].as_str().expect("leaf")
            ))
            .await;
        assert_eq!(path["leaf_index"], index);
        assert_eq!(root_of(&path), ALLOWLIST_ROOT);
    }
    let (status, _) = fx
        .get(&format!("/v1/allowlist/leaves/0x{}/path", "22".repeat(32)))
        .await;
    assert_eq!(status, 404);
    fx.drop().await;
}

#[tokio::test]
async fn an_operator_maintained_allowlist_serves_no_paths() {
    let Some(fx) = setup(None).await else { return };
    let leaf = fx.ok("/v1/allowlist/entries?limit=1").await["items"][0]["leaf"]
        .as_str()
        .expect("leaf")
        .to_owned();
    let toid = 5_100_000i64 << 32 | 1;
    sqlx::query(
        "INSERT INTO contract_events (toid, event_index, ledger, contract, source, name, topics_xdr, data_xdr) \
         VALUES ($1, 0, 5100000, $2, 'rpc', 'LeafRevoked', '{}', '')",
    )
    .bind(toid)
    .bind(ALLOWLIST)
    .execute(fx.writer.pool())
    .await
    .expect("raw");
    sqlx::query(
        "INSERT INTO allowlist_events (toid, event_index, allowlist, kind, leaf, root, ledger) \
         VALUES ($1, 0, $2, 'revoked', $3, $4, 5100000)",
    )
    .bind(toid)
    .bind(ALLOWLIST)
    .bind(bytes(&leaf))
    .bind(vec![3u8; 32])
    .execute(fx.writer.pool())
    .await
    .expect("revocation");

    let body = fx.ok("/v1/allowlist").await;
    assert_eq!(body["operator_maintained"], true);
    assert_eq!(body["revocations"], 1);
    let (status, body) = fx.get(&format!("/v1/allowlist/leaves/{leaf}/path")).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"]["code"], "conflict");
    fx.drop().await;
}

#[tokio::test]
async fn the_api_cannot_write_to_the_database() {
    let Some(fx) = setup(None).await else { return };
    let read_only = veil_api::read_only_pool(fx.admin.clone().database(&fx.name), 1)
        .await
        .expect("pool");
    let error = sqlx::query("DELETE FROM pool_commitments")
        .execute(&read_only)
        .await
        .expect_err("refused");
    assert!(
        error.to_string().contains("read-only transaction"),
        "{error}"
    );
    read_only.close().await;
    fx.drop().await;
}

#[tokio::test]
async fn bad_pages_unknown_routes_and_request_ids_are_handled() {
    let Some(fx) = setup(None).await else { return };
    for path in [
        "/v1/pool/leaves?limit=0",
        "/v1/pool/leaves?limit=1001",
        "/v1/notes?limit=0",
        "/v1/allowlist/entries?cursor=nope",
    ] {
        let (status, body) = fx.get(path).await;
        assert_eq!(status, 400, "{path}");
        assert_eq!(body["error"]["code"], "bad_request", "{path}");
    }
    let (status, body) = fx.get("/v2/anything").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");

    // A request id is generated, or the caller's is echoed back.
    let response = fx
        .http
        .get(format!("{}/v1/status", fx.base))
        .send()
        .await
        .expect("request");
    assert!(response.headers().get("x-request-id").is_some());
    let response = fx
        .http
        .get(format!("{}/v1/status", fx.base))
        .header("x-request-id", "trace-me")
        .send()
        .await
        .expect("request");
    assert_eq!(response.headers()["x-request-id"], "trace-me");

    // Any origin may read: the data is public and the API is read-only.
    let response = fx
        .http
        .get(format!("{}/v1/status", fx.base))
        .header("origin", "https://example.org")
        .send()
        .await
        .expect("request");
    assert_eq!(response.headers()["access-control-allow-origin"], "*");
    fx.drop().await;
}

#[tokio::test]
async fn readiness_follows_the_indexer() {
    let Some(fx) = setup(None).await else { return };
    let (status, _) = fx.get("/ready").await;
    assert_eq!(status, 200);
    sqlx::query(
        "UPDATE ingest_checkpoints SET updated_at = now() - interval '1 hour' WHERE stream = 'rpc'",
    )
    .execute(fx.writer.pool())
    .await
    .expect("age the checkpoint");
    let (status, body) = fx.get("/ready").await;
    assert_eq!(status, 503);
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("last advanced"))
    );
    let (status, _) = fx.get("/health").await;
    assert_eq!(status, 200);
    fx.drop().await;
}

#[tokio::test]
async fn a_client_over_its_rate_is_refused() {
    let Some(fx) = setup(Some((1, 3))).await else {
        return;
    };
    let mut codes = Vec::new();
    for _ in 0..6 {
        codes.push(fx.get("/v1/status").await.0);
    }
    assert_eq!(&codes[..3], [200, 200, 200]);
    assert!(codes[3..].contains(&429), "{codes:?}");
    fx.drop().await;
}

#[tokio::test]
async fn the_openapi_document_lists_every_route() {
    let Some(fx) = setup(None).await else { return };
    let doc = fx.ok("/openapi.json").await;
    let paths = doc["paths"].as_object().expect("paths");
    for path in [
        "/v1/status",
        "/v1/pool",
        "/v1/pool/leaves",
        "/v1/pool/leaves/{leaf_index}/path",
        "/v1/pool/nullifiers/{nullifier}",
        "/v1/notes",
        "/v1/public-keys/{owner}",
        "/v1/allowlist",
        "/v1/allowlist/entries",
        "/v1/allowlist/leaves/{leaf}/path",
    ] {
        assert!(
            paths.contains_key(path),
            "{path} missing from the OpenAPI document"
        );
    }
    assert_eq!(paths.len(), 10);
    fx.drop().await;
}
