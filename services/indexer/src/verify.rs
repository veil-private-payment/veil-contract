//! Checks the stored index against the contracts' on-chain state.
//!
//! The pool's tree is rebuilt from the stored commitments and compared with
//! the root and size the pool holds. The allowlist is replayed insertion by
//! insertion, checking every root it emitted. Once a revocation hands the
//! allowlist to its operator, roots are computed off chain and published, so
//! from there the check is that the latest published root is the one stored.
//!
//! State is read first, then the database is read as of that same ledger, so
//! the two sides describe one moment even while the indexer keeps running.

use crate::chain::{AllowlistTree, ChainError, PoolTree};
use crate::event::U256Be;
use crate::ingest::{Contracts, RPC_STREAM};
use crate::rpc::RpcClient;
use crate::store::{Store, StoreError};
use crate::tree::{Field, IncrementalTree, TreeError, root};

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error(
        "the index has reached ledger {indexed}, but the state was read at {state}: \
         the indexer is behind"
    )]
    Behind { indexed: u32, state: u32 },
    #[error("stored {what} {value} is malformed")]
    Malformed { what: &'static str, value: String },
}

impl Report {
    fn matches(&self, prefix: &str) -> bool {
        self.checks
            .iter()
            .filter(|check| check.name.starts_with(prefix))
            .all(Check::ok)
    }

    /// Records the on-chain state and whether the index matched it, for the
    /// API to show beside its own roots.
    pub async fn record(&self, store: &Store, contracts: &Contracts) -> Result<(), VerifyError> {
        let rows = [
            (
                &contracts.pool,
                "pool",
                self.pool.ledger,
                self.pool.levels,
                self.pool.next_index,
                self.pool.root,
                false,
                self.matches("pool:"),
            ),
            (
                &contracts.allowlist,
                "allowlist",
                self.allowlist.ledger,
                self.allowlist.levels,
                self.allowlist.next_index,
                self.allowlist.root,
                self.allowlist.operator_maintained,
                self.matches("allowlist:"),
            ),
        ];
        for (contract, kind, ledger, levels, next_index, root, operator, matches) in rows {
            let next_index = i64::try_from(next_index).map_err(|_| VerifyError::Malformed {
                what: "next index",
                value: next_index.to_string(),
            })?;
            sqlx::query(
                "INSERT INTO verified_state \
                     (contract, kind, ledger, levels, next_index, root, operator_maintained, matches) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (contract) DO UPDATE SET \
                     kind = EXCLUDED.kind, ledger = EXCLUDED.ledger, levels = EXCLUDED.levels, \
                     next_index = EXCLUDED.next_index, root = EXCLUDED.root, \
                     operator_maintained = EXCLUDED.operator_maintained, \
                     matches = EXCLUDED.matches, checked_at = now()",
            )
            .bind(contract)
            .bind(kind)
            .bind(i64::from(ledger))
            .bind(i64::from(levels))
            .bind(next_index)
            .bind(root.as_bytes().as_slice())
            .bind(operator)
            .bind(matches)
            .execute(store.pool())
            .await?;
        }
        Ok(())
    }
}

/// One comparison between the index and the chain.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Check {
    pub name: String,
    pub indexed: String,
    pub on_chain: String,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.indexed == self.on_chain
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    pub pool_ledger: u32,
    pub allowlist_ledger: u32,
    /// The on-chain state the index was compared with.
    pub pool: PoolTree,
    pub allowlist: AllowlistTree,
    pub checks: Vec<Check>,
    pub evidence: Evidence,
}

/// What a reviewer needs to read the report on its own: which network and
/// contracts, which build, where the stored events came from, and how the
/// pool's tree size breaks down.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Evidence {
    /// As RPC reported it, so the network is confirmed by the chain.
    pub network_passphrase: String,
    pub pool: String,
    pub allowlist: String,
    pub indexer_version: &'static str,
    /// Stored events per contract and source, up to the pool state's ledger.
    pub events_by_source: Vec<SourceCount>,
    /// Commitment rows stored for the pool.
    pub pool_commitment_rows: u64,
    /// Tree positions holding the zero leaf the closed deposit path inserted
    /// beside each deposit, which emitted no event. Rows plus these equal the
    /// pool's next index.
    pub pool_legacy_zero_positions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceCount {
    pub contract: String,
    /// `stellar_expert` or `rpc`.
    pub source: String,
    pub events: u64,
}

async fn evidence(
    rpc: &RpcClient,
    store: &Store,
    contracts: &Contracts,
    ledger: u32,
) -> Result<Evidence, VerifyError> {
    let network_passphrase = rpc.network_passphrase().await.map_err(ChainError::from)?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT contract, source, count(*) FROM contract_events \
         WHERE ledger <= $1 AND contract IN ($2, $3) GROUP BY contract, source \
         ORDER BY contract, source",
    )
    .bind(i64::from(ledger))
    .bind(&contracts.pool)
    .bind(&contracts.allowlist)
    .fetch_all(store.pool())
    .await?;
    let events_by_source = rows
        .into_iter()
        .map(|(contract, source, events)| {
            Ok(SourceCount {
                contract,
                source,
                events: index(events, "event count")?,
            })
        })
        .collect::<Result<Vec<_>, VerifyError>>()?;
    let (commitments, zeros): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE origin = 'legacy_deposit') \
         FROM pool_commitments WHERE pool = $1 AND ledger <= $2",
    )
    .bind(&contracts.pool)
    .bind(i64::from(ledger))
    .fetch_one(store.pool())
    .await?;
    Ok(Evidence {
        network_passphrase,
        pool: contracts.pool.clone(),
        allowlist: contracts.allowlist.clone(),
        indexer_version: env!("CARGO_PKG_VERSION"),
        events_by_source,
        pool_commitment_rows: index(commitments, "commitment count")?,
        pool_legacy_zero_positions: index(zeros, "deposit count")?,
    })
}

impl Report {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(Check::ok)
    }
}

fn check(name: impl Into<String>, indexed: impl ToString, on_chain: impl ToString) -> Check {
    Check {
        name: name.into(),
        indexed: indexed.to_string(),
        on_chain: on_chain.to_string(),
    }
}

/// The last ledger the index holds completely. A cursor inside a ledger (a
/// full page) covers only the ledgers before it.
async fn indexed_ledger(store: &Store) -> Result<u32, VerifyError> {
    Ok(store.checkpoint(RPC_STREAM).await?.map_or(0, |checkpoint| {
        let ledger = checkpoint.cursor.ledger();
        if checkpoint.cursor.event_index() == u32::MAX {
            ledger
        } else {
            ledger.saturating_sub(1)
        }
    }))
}

/// Reads both contracts' state and compares it with the index. If the index
/// has not yet reached the ledger the state was read at, waits up to `wait`
/// for the running indexer to catch up.
pub async fn verify(
    rpc: &RpcClient,
    store: &Store,
    contracts: &Contracts,
    wait: std::time::Duration,
) -> Result<Report, VerifyError> {
    let pool = rpc.pool_tree(&contracts.pool).await?;
    let allowlist = rpc.allowlist_tree(&contracts.allowlist).await?;
    let newest = pool.ledger.max(allowlist.ledger);
    let start = tokio::time::Instant::now();
    loop {
        let indexed = indexed_ledger(store).await?;
        if indexed >= newest {
            break;
        }
        if start.elapsed() >= wait {
            return Err(VerifyError::Behind {
                indexed,
                state: newest,
            });
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    let mut checks = check_pool(store, &contracts.pool, &pool).await?;
    checks.extend(check_allowlist(store, &contracts.allowlist, &allowlist).await?);
    let evidence = evidence(rpc, store, contracts, pool.ledger).await?;
    Ok(Report {
        pool_ledger: pool.ledger,
        allowlist_ledger: allowlist.ledger,
        pool,
        allowlist,
        checks,
        evidence,
    })
}

fn u256(bytes: &[u8], what: &'static str) -> Result<U256Be, VerifyError> {
    let array: [u8; 32] = bytes.try_into().map_err(|_| VerifyError::Malformed {
        what,
        value: format!("{} bytes", bytes.len()),
    })?;
    Ok(U256Be(array))
}

fn index(value: i64, what: &'static str) -> Result<u64, VerifyError> {
    u64::try_from(value).map_err(|_| VerifyError::Malformed {
        what,
        value: value.to_string(),
    })
}

/// The pool's tree as of `state.ledger`.
///
/// The leaves are the stored commitments at their indices. The closed deposit
/// path inserted each commitment paired with a zero leaf at the next index,
/// which emitted no event, so that position is filled with zero here. Every
/// position below the on-chain `NextIndex` must then be accounted for.
pub async fn check_pool(
    store: &Store,
    pool: &str,
    state: &PoolTree,
) -> Result<Vec<Check>, VerifyError> {
    let rows: Vec<(i64, Vec<u8>, String)> = sqlx::query_as(
        "SELECT leaf_index, commitment, origin FROM pool_commitments \
         WHERE pool = $1 AND ledger <= $2 ORDER BY leaf_index",
    )
    .bind(pool)
    .bind(i64::from(state.ledger))
    .fetch_all(store.pool())
    .await?;

    let mut leaves = Vec::with_capacity(rows.len().saturating_mul(2));
    for (leaf_index, commitment, origin) in rows {
        let leaf_index = index(leaf_index, "leaf index")?;
        leaves.push((
            leaf_index,
            Field::from_u256(&u256(&commitment, "commitment")?)?,
        ));
        if origin == "legacy_deposit" {
            leaves.push((leaf_index.saturating_add(1), Field::zero()));
        }
    }
    let next_index = leaves
        .iter()
        .map(|(i, _)| i.saturating_add(1))
        .max()
        .unwrap_or(0);
    let mut sorted: Vec<u64> = leaves.iter().map(|(i, _)| *i).collect();
    sorted.sort_unstable();
    let contiguous = sorted
        .iter()
        .zip(0u64..)
        .all(|(found, expected)| *found == expected);
    let computed = root(state.levels, &leaves)?;

    Ok(vec![
        check("pool: tree positions 0..n all filled", contiguous, true),
        check("pool: next index", next_index, state.next_index),
        check("pool: root", computed.to_u256(), state.root),
    ])
}

/// kind, leaf, leaf_index, root
type AllowlistRow = (String, Vec<u8>, Option<i64>, Vec<u8>);

/// The allowlist as of `state.ledger`.
pub async fn check_allowlist(
    store: &Store,
    allowlist: &str,
    state: &AllowlistTree,
) -> Result<Vec<Check>, VerifyError> {
    let rows: Vec<AllowlistRow> = sqlx::query_as(
        "SELECT kind, leaf, leaf_index, root FROM allowlist_events \
         WHERE allowlist = $1 AND ledger <= $2 ORDER BY toid, event_index",
    )
    .bind(allowlist)
    .bind(i64::from(state.ledger))
    .fetch_all(store.pool())
    .await?;

    let mut tree = IncrementalTree::new(state.levels)?;
    let mut replayed = 0u64;
    let mut emitted_roots_match = true;
    let mut handed_to_operator = false;
    let mut latest_root = tree.root().to_u256();
    for (kind, leaf, leaf_index, emitted_root) in rows {
        let emitted_root = u256(&emitted_root, "allowlist root")?;
        latest_root = emitted_root;
        if kind == "revoked" {
            handed_to_operator = true;
        }
        if handed_to_operator {
            continue;
        }
        // Before any revocation, insertions are contiguous and each emitted
        // root is the one the contract computed.
        let leaf_index = leaf_index
            .map(|i| index(i, "allowlist index"))
            .transpose()?;
        if leaf_index != Some(tree.next_index()) {
            emitted_roots_match = false;
        }
        let computed = tree.push(Field::from_u256(&u256(&leaf, "allowlist leaf")?)?)?;
        if computed.to_u256() != emitted_root {
            emitted_roots_match = false;
        }
        replayed = replayed.saturating_add(1);
    }

    let mut checks = vec![
        check(
            "allowlist: every root emitted before a revocation recomputes",
            emitted_roots_match,
            true,
        ),
        check(
            "allowlist: operator-maintained",
            handed_to_operator,
            state.operator_maintained,
        ),
        check("allowlist: latest emitted root", latest_root, state.root),
    ];
    if !handed_to_operator {
        checks.push(check(
            "allowlist: recomputed root",
            tree.root().to_u256(),
            state.root,
        ));
        checks.push(check("allowlist: next index", replayed, state.next_index));
    }
    Ok(checks)
}
