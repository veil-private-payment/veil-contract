//! Shared helpers for the database tests: a throwaway database per test, the
//! real testnet history as fixtures, and an event source that serves that
//! history the way Soroban RPC pages it.

#![allow(dead_code)]

use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, Executor};
use veil_indexer::ingest::Contracts;
use veil_indexer::position::EventPosition;
use veil_indexer::rpc::{EventSource, EventsPage, RpcError, Start};
use veil_indexer::source::{ContractEvent, ExpertEventRecord, Source};
use veil_indexer::store::Store;

pub const POOL: &str = "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G";
pub const ALLOWLIST: &str = "CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB";

pub fn contracts() -> Contracts {
    Contracts {
        pool: POOL.to_owned(),
        allowlist: ALLOWLIST.to_owned(),
    }
}

/// A database created for one test and dropped by [`TestDb::drop`].
///
/// Needs `VEIL_TEST_DATABASE_URL` pointing at a Postgres the tests may create
/// databases on, such as the one in `services/indexer/docker-compose.yml`.
/// Without it the database tests are skipped locally and fail under CI.
pub struct TestDb {
    pub store: Store,
    admin: PgConnectOptions,
    name: String,
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

impl TestDb {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("VEIL_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "VEIL_TEST_DATABASE_URL must be set in CI"
            );
            eprintln!("VEIL_TEST_DATABASE_URL is not set, skipping a database test");
            return None;
        };
        let admin = PgConnectOptions::from_str(&url).expect("VEIL_TEST_DATABASE_URL parses");
        let name = format!(
            "veil_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut connection = admin.connect().await.expect("admin connection");
        connection
            .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
            .await
            .expect("drop stale test database");
        connection
            .execute(format!("CREATE DATABASE {name}").as_str())
            .await
            .expect("create test database");
        connection.close().await.expect("close admin connection");

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(admin.clone().database(&name))
            .await
            .expect("connect to test database");
        Some(Self {
            store: Store::from_pool(pool),
            admin,
            name,
        })
    }

    /// A migrated database.
    pub async fn migrated() -> Option<Self> {
        let db = Self::new().await?;
        db.store.migrate().await.expect("migrations apply");
        Some(db)
    }

    /// Another store on the same database, as a second instance would have.
    pub async fn second_store(&self) -> Store {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(self.admin.clone().database(&self.name))
            .await
            .expect("connect to test database");
        Store::from_pool(pool)
    }

    pub async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(self.store.pool())
            .await
            .expect("count rows")
    }

    /// Every stored row, in a stable order, for comparing two databases.
    pub async fn dump(&self) -> Vec<String> {
        let mut rows = Vec::new();
        for (table, order) in [
            ("contract_events", "toid, event_index"),
            ("pool_commitments", "toid, event_index"),
            ("pool_nullifiers", "toid, event_index"),
            ("pool_settlements", "toid, event_index"),
            ("pool_exits", "toid, event_index"),
            ("pool_legacy_deposits", "toid, event_index"),
            ("pool_public_keys", "toid, event_index"),
            ("allowlist_events", "toid, event_index"),
        ] {
            // Everything except the wall-clock column.
            let query = format!(
                "SELECT '{table}' || ':' || (to_jsonb(t) - 'stored_at')::text \
                 FROM {table} t ORDER BY {order}"
            );
            let mut table_rows: Vec<String> = sqlx::query_scalar(&query)
                .fetch_all(self.store.pool())
                .await
                .expect("dump table");
            rows.append(&mut table_rows);
        }
        rows
    }

    pub async fn drop(self) {
        self.store.pool().close().await;
        let mut connection = self.admin.connect().await.expect("admin connection");
        connection
            .execute(format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name).as_str())
            .await
            .expect("drop test database");
    }
}

/// The pool's and the allowlist's full testnet history, merged in ledger
/// order, as RPC would return it for a filter on both contracts.
pub fn history() -> Vec<ContractEvent> {
    let mut events: Vec<ContractEvent> = ["pool_CDJELV6H.json", "asp_CBRZ7RD5.json"]
        .into_iter()
        .flat_map(|name| {
            let path = format!(
                "{}/tests/fixtures/stellar_expert/{name}",
                env!("CARGO_MANIFEST_DIR")
            );
            let text = std::fs::read_to_string(&path).expect("fixture exists");
            let records: Vec<ExpertEventRecord> =
                serde_json::from_str(&text).expect("fixture is JSON");
            records
                .into_iter()
                .map(|record| record.into_event().expect("record parses"))
        })
        .collect();
    events.sort_by_key(|event| event.position);
    events
}

/// The end-of-scan cursor RPC returns for a page that stopped at `ledger`.
pub fn end_of_ledger(ledger: u32) -> EventPosition {
    EventPosition::new((u64::from(ledger) << 32) | u64::from(u32::MAX), u32::MAX)
}

/// Serves `events` the way `getEvents` pages them: at most `limit` events,
/// at most `window` ledgers scanned per request, and a cursor that is the
/// last event of a full page or the end of the scanned range otherwise.
pub struct FakeRpc {
    events: Vec<ContractEvent>,
    latest_ledger: u32,
    window: u32,
    /// Fail every request after this many have succeeded.
    fail_after: Option<usize>,
    served: AtomicUsize,
    pub requests: Mutex<Vec<Start>>,
}

impl FakeRpc {
    pub fn new(events: Vec<ContractEvent>) -> Self {
        let events: Vec<ContractEvent> = events
            .into_iter()
            .map(|event| ContractEvent {
                source: Source::Rpc,
                ..event
            })
            .collect();
        let last = events.last().map_or(1, |event| event.position.ledger());
        Self {
            events,
            latest_ledger: last.saturating_add(50),
            window: 5_000,
            fail_after: None,
            served: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn failing_after(mut self, pages: usize) -> Self {
        self.fail_after = Some(pages);
        self
    }

    pub fn requests(&self) -> Vec<Start> {
        self.requests.lock().expect("lock").clone()
    }
}

impl EventSource for FakeRpc {
    async fn events(
        &self,
        start: Start,
        contracts: &[String],
        limit: u32,
    ) -> Result<EventsPage, RpcError> {
        self.requests.lock().expect("lock").push(start);
        if self
            .fail_after
            .is_some_and(|after| self.served.load(Ordering::SeqCst) >= after)
        {
            return Err(RpcError::Rpc {
                code: -32603,
                message: "injected failure".to_owned(),
            });
        }
        self.served.fetch_add(1, Ordering::SeqCst);

        let (first_ledger, after) = match start {
            Start::Ledger(ledger) => (ledger, None),
            Start::After(cursor) => (cursor.ledger(), Some(cursor)),
        };
        let scan_end = first_ledger
            .saturating_add(self.window)
            .min(self.latest_ledger);
        let limit = usize::try_from(limit).expect("limit fits");
        let page: Vec<ContractEvent> = self
            .events
            .iter()
            .filter(|event| {
                event.position.ledger() >= first_ledger
                    && event.position.ledger() <= scan_end
                    && after.is_none_or(|after| event.position > after)
                    && contracts.contains(&event.contract)
            })
            .take(limit)
            .cloned()
            .collect();
        let cursor = match page.last() {
            Some(last) if page.len() == limit => last.position,
            _ => end_of_ledger(scan_end),
        };
        Ok(EventsPage {
            events: page,
            cursor,
            latest_ledger: self.latest_ledger,
            oldest_ledger: 1,
        })
    }
}
