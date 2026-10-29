//! Database reads.
//!
//! Requests that combine several reads (a tree and the ledger it reflects)
//! run them in one repeatable-read transaction, so the answer describes one
//! moment even while the indexer writes.

use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use veil_indexer::event::U256Be;
use veil_indexer::tree::Field;

use crate::error::ApiError;
use crate::model::OnChain;

/// RFC 3339 in UTC, as text, so no date-time crate is needed.
const RFC3339: &str = r#"'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'"#;

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2).saturating_add(2));
    out.push_str("0x");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn u256(bytes: &[u8]) -> Result<U256Be, ApiError> {
    let array: [u8; 32] = bytes.try_into().map_err(|_| {
        ApiError::Internal(anyhow::anyhow!("stored value is {} bytes", bytes.len()))
    })?;
    Ok(U256Be(array))
}

fn non_negative<T: TryFrom<i64>>(value: i64, what: &str) -> Result<T, ApiError> {
    T::try_from(value).map_err(|_| ApiError::Internal(anyhow::anyhow!("stored {what} {value}")))
}

fn ledger(value: i32) -> Result<u32, ApiError> {
    non_negative(i64::from(value), "ledger")
}

/// A read-only, repeatable-read transaction. Dropping it rolls back, so an
/// error mid-request never returns a connection to the pool mid-transaction.
pub async fn snapshot(pool: &PgPool) -> Result<Transaction<'static, Postgres>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The RPC checkpoint: (last complete ledger, latest ledger, updated at).
pub async fn checkpoint(
    connection: &mut PgConnection,
) -> Result<Option<(u32, u32, String)>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT cursor_toid, cursor_event, latest_ledger, \
         to_char(updated_at AT TIME ZONE 'UTC', {RFC3339}) AS updated_at \
         FROM ingest_checkpoints WHERE stream = 'rpc'"
    ))
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let toid: u64 = non_negative(row.try_get::<i64, _>("cursor_toid")?, "cursor")?;
    let event: i64 = row.try_get("cursor_event")?;
    let cursor_ledger = u32::try_from(toid >> 32).unwrap_or(u32::MAX);
    // A cursor inside a ledger covers only the ledgers before it.
    let complete = if event == i64::from(u32::MAX) {
        cursor_ledger
    } else {
        cursor_ledger.saturating_sub(1)
    };
    Ok(Some((
        complete,
        ledger(row.try_get("latest_ledger")?)?,
        row.try_get("updated_at")?,
    )))
}

pub async fn indexed_ledger(connection: &mut PgConnection) -> Result<Option<u32>, ApiError> {
    Ok(checkpoint(connection).await?.map(|(ledger, _, _)| ledger))
}

pub async fn on_chain(
    connection: &mut PgConnection,
    contract: &str,
) -> Result<Option<(OnChain, bool)>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT ledger, levels, next_index, root, matches, operator_maintained, \
         to_char(checked_at AT TIME ZONE 'UTC', {RFC3339}) AS checked_at \
         FROM verified_state WHERE contract = $1"
    ))
    .bind(contract)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let root: Vec<u8> = row.try_get("root")?;
    Ok(Some((
        OnChain {
            ledger: ledger(row.try_get("ledger")?)?,
            levels: ledger(row.try_get("levels")?)?,
            next_index: non_negative(row.try_get::<i64, _>("next_index")?, "next index")?,
            root: hex(&root),
            matches: row.try_get("matches")?,
            checked_at: row.try_get("checked_at")?,
        },
        row.try_get("operator_maintained")?,
    )))
}

/// The tree depth the indexer read from `contract`, needed for any root.
pub async fn levels(connection: &mut PgConnection, contract: &str) -> Result<u32, ApiError> {
    on_chain(connection, contract)
        .await?
        .map(|(state, _)| state.levels)
        .ok_or_else(|| {
            ApiError::Unavailable(
                "the indexer has not verified this contract yet, so its tree depth is unknown"
                    .to_owned(),
            )
        })
}

/// One leaf of the pool tree with its origin and ledger.
pub struct PoolLeaf {
    pub index: u64,
    pub value: U256Be,
    pub origin: &'static str,
    pub ledger: u32,
}

/// Pool leaves with `from <= index`, in order, at most `limit`. The zero leaf
/// beside each legacy deposit is included, as the contract inserted it.
pub async fn pool_leaves(
    connection: &mut PgConnection,
    pool: &str,
    from: u64,
    limit: Option<u64>,
) -> Result<Vec<PoolLeaf>, ApiError> {
    // A deposit at index i also filled i + 1, so start one row earlier.
    let start = i64::try_from(from.saturating_sub(1)).unwrap_or(i64::MAX);
    let rows = sqlx::query(
        "SELECT leaf_index, commitment, origin, ledger FROM pool_commitments \
         WHERE pool = $1 AND leaf_index >= $2 ORDER BY leaf_index LIMIT $3",
    )
    .bind(pool)
    .bind(start)
    .bind(limit.map_or(i64::MAX, |limit| {
        i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
    }))
    .fetch_all(&mut *connection)
    .await?;

    let mut leaves = Vec::with_capacity(rows.len().saturating_mul(2));
    for row in rows {
        let index: u64 = non_negative(row.try_get::<i64, _>("leaf_index")?, "leaf index")?;
        let value = u256(&row.try_get::<Vec<u8>, _>("commitment")?)?;
        let origin: String = row.try_get("origin")?;
        let at = ledger(row.try_get("ledger")?)?;
        let legacy = origin == "legacy_deposit";
        leaves.push(PoolLeaf {
            index,
            value,
            origin: if legacy { "legacy_deposit" } else { "transact" },
            ledger: at,
        });
        if legacy {
            leaves.push(PoolLeaf {
                index: index.saturating_add(1),
                value: U256Be([0; 32]),
                origin: "legacy_deposit_zero",
                ledger: at,
            });
        }
    }
    leaves.retain(|leaf| leaf.index >= from);
    if let Some(limit) = limit {
        leaves.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    Ok(leaves)
}

pub fn as_tree(leaves: &[PoolLeaf]) -> Result<Vec<(u64, Field)>, ApiError> {
    leaves
        .iter()
        .map(|leaf| {
            Field::from_u256(&leaf.value)
                .map(|field| (leaf.index, field))
                .map_err(|err| ApiError::Internal(err.into()))
        })
        .collect()
}

/// (ledger, event position) of the spend, if spent.
pub async fn nullifier(
    connection: &mut PgConnection,
    pool: &str,
    nullifier: &U256Be,
) -> Result<Option<(u32, String)>, ApiError> {
    let row = sqlx::query(
        "SELECT ledger, toid, event_index FROM pool_nullifiers WHERE pool = $1 AND nullifier = $2",
    )
    .bind(pool)
    .bind(nullifier.as_bytes().as_slice())
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let toid: u64 = non_negative(row.try_get::<i64, _>("toid")?, "toid")?;
    let event: u32 = non_negative(
        i64::from(row.try_get::<i32, _>("event_index")?),
        "event index",
    )?;
    Ok(Some((
        ledger(row.try_get("ledger")?)?,
        veil_indexer::position::EventPosition::new(toid, event).to_string(),
    )))
}

/// Notes of `transact` commitments with `from <= index`, in order.
pub async fn notes(
    connection: &mut PgConnection,
    pool: &str,
    from: u64,
    limit: u64,
) -> Result<Vec<(u64, U256Be, Vec<u8>, u32)>, ApiError> {
    let rows = sqlx::query(
        "SELECT leaf_index, commitment, encrypted_output, ledger FROM pool_commitments \
         WHERE pool = $1 AND origin = 'transact' AND leaf_index >= $2 \
         ORDER BY leaf_index LIMIT $3",
    )
    .bind(pool)
    .bind(i64::try_from(from).unwrap_or(i64::MAX))
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                non_negative(row.try_get::<i64, _>("leaf_index")?, "leaf index")?,
                u256(&row.try_get::<Vec<u8>, _>("commitment")?)?,
                row.try_get::<Option<Vec<u8>>, _>("encrypted_output")?
                    .unwrap_or_default(),
                ledger(row.try_get("ledger")?)?,
            ))
        })
        .collect()
}

/// The latest registration of `owner`: (encryption key, note key, ledger,
/// number of registrations).
pub async fn public_key(
    connection: &mut PgConnection,
    pool: &str,
    owner: &str,
) -> Result<Option<(Vec<u8>, Vec<u8>, u32, u64)>, ApiError> {
    let row = sqlx::query(
        "SELECT encryption_key, note_key, ledger, count(*) OVER () AS registrations \
         FROM pool_public_keys WHERE pool = $1 AND owner = $2 \
         ORDER BY toid DESC, event_index DESC LIMIT 1",
    )
    .bind(pool)
    .bind(owner)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some((
        row.try_get("encryption_key")?,
        row.try_get("note_key")?,
        ledger(row.try_get("ledger")?)?,
        non_negative(row.try_get::<i64, _>("registrations")?, "registrations")?,
    )))
}

/// One allowlist change: kind, leaf, index, root, ledger, position.
pub struct AllowlistRow {
    pub kind: String,
    pub leaf: U256Be,
    pub leaf_index: Option<u64>,
    pub root: U256Be,
    pub ledger: u32,
    pub position: veil_indexer::position::EventPosition,
}

/// Allowlist changes after `after`, in order, at most `limit`.
pub async fn allowlist(
    connection: &mut PgConnection,
    allowlist: &str,
    after: Option<veil_indexer::position::EventPosition>,
    limit: Option<u64>,
) -> Result<Vec<AllowlistRow>, ApiError> {
    let (toid, event) = after.map_or((-1, -1), |position| {
        (
            i64::try_from(position.toid()).unwrap_or(i64::MAX),
            i64::from(position.event_index()),
        )
    });
    let rows = sqlx::query(
        "SELECT kind, leaf, leaf_index, root, ledger, toid, event_index FROM allowlist_events \
         WHERE allowlist = $1 AND (toid, event_index) > ($2, $3) \
         ORDER BY toid, event_index LIMIT $4",
    )
    .bind(allowlist)
    .bind(toid)
    .bind(event)
    .bind(limit.map_or(i64::MAX, |limit| i64::try_from(limit).unwrap_or(i64::MAX)))
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(|row| {
            let toid: u64 = non_negative(row.try_get::<i64, _>("toid")?, "toid")?;
            let event: u32 = non_negative(
                i64::from(row.try_get::<i32, _>("event_index")?),
                "event index",
            )?;
            Ok(AllowlistRow {
                kind: row.try_get("kind")?,
                leaf: u256(&row.try_get::<Vec<u8>, _>("leaf")?)?,
                leaf_index: row
                    .try_get::<Option<i64>, _>("leaf_index")?
                    .map(|index| non_negative(index, "leaf index"))
                    .transpose()?,
                root: u256(&row.try_get::<Vec<u8>, _>("root")?)?,
                ledger: ledger(row.try_get("ledger")?)?,
                position: veil_indexer::position::EventPosition::new(toid, event),
            })
        })
        .collect()
}
