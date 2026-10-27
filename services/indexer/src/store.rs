//! The indexer's database: migrations, idempotent writes and checkpoints.
//!
//! Every write is keyed by the position of the event that produced it, so a
//! batch can be written any number of times and leaves the same rows. A batch
//! and the checkpoint that covers it commit in one transaction: after a crash
//! the database holds either the whole batch and its checkpoint or neither.
//!
//! Anything that contradicts what is already stored is an error, never a
//! silent skip: the same position with different XDR, a tree index or a
//! nullifier that a different event already claimed, or a checkpoint moving
//! backwards. Each of those means the index is wrong, and the transaction is
//! rolled back so nothing from that batch is kept.

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Row, Transaction};
use stellar_xdr::curr::{Limits, WriteXdr};

use crate::config::Secret;
use crate::event::{Decoded, U256Be, VeilEvent};
use crate::position::EventPosition;
use crate::source::{ContractEvent, Source};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Why a database operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{what} {value} does not fit the column it is stored in")]
    OutOfRange { what: &'static str, value: String },
    #[error("encoding event {position} back to XDR: {reason}")]
    Encode {
        position: EventPosition,
        reason: String,
    },
    #[error(
        "event {position} is already stored with different content \
         (stored from {stored_source}, now read from {incoming_source:?})"
    )]
    Conflict {
        position: EventPosition,
        stored_source: String,
        incoming_source: Source,
    },
    #[error("checkpoint for {stream} would move back from {stored} to {proposed}")]
    CheckpointRegression {
        stream: String,
        stored: EventPosition,
        proposed: EventPosition,
    },
}

/// How far an ingestion stream has read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// The cursor to resume from. Every event at or before it is stored.
    pub cursor: EventPosition,
    /// Latest ledger the source reported when the cursor was written.
    pub latest_ledger: u32,
}

/// What one batch changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Events stored for the first time.
    pub inserted: usize,
    /// Events that were already stored with identical content.
    pub already_stored: usize,
}

/// One event ready to store: where it came from and what it decoded to.
#[derive(Clone, Debug)]
pub struct StoredEvent {
    pub event: ContractEvent,
    pub decoded: Decoded,
}

/// Connection options for [`Store::connect`].
#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_connections: 5,
            acquire_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub async fn connect(url: &Secret, options: &StoreOptions) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.acquire_timeout)
            .connect(url.expose())
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies any migration not yet applied.
    ///
    /// sqlx holds a Postgres advisory lock for the duration, so instances
    /// starting together apply each migration once and the others wait.
    /// A migration file edited after it was applied fails the checksum check
    /// and stops start-up.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub async fn checkpoint(&self, stream: &str) -> Result<Option<Checkpoint>, StoreError> {
        let row = sqlx::query(
            "SELECT cursor_toid, cursor_event, latest_ledger \
             FROM ingest_checkpoints WHERE stream = $1",
        )
        .bind(stream)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| read_checkpoint(&row)).transpose()
    }

    /// Stores `events` and moves `stream`'s checkpoint to `checkpoint`, in one
    /// transaction.
    ///
    /// Events are written in the order given, which must be ledger order so
    /// that foreign keys between them hold.
    pub async fn write_batch(
        &self,
        stream: &str,
        events: &[StoredEvent],
        checkpoint: Checkpoint,
    ) -> Result<BatchOutcome, StoreError> {
        let mut tx = self.pool.begin().await?;

        // Lock the checkpoint row first, so two writers on the same stream
        // serialise here rather than racing on the event rows.
        let stored = sqlx::query(
            "SELECT cursor_toid, cursor_event, latest_ledger \
             FROM ingest_checkpoints WHERE stream = $1 FOR UPDATE",
        )
        .bind(stream)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| read_checkpoint(&row))
        .transpose()?;
        if let Some(stored) = stored
            && checkpoint.cursor < stored.cursor
        {
            return Err(StoreError::CheckpointRegression {
                stream: stream.to_owned(),
                stored: stored.cursor,
                proposed: checkpoint.cursor,
            });
        }

        let mut outcome = BatchOutcome::default();
        for stored_event in events {
            if insert_event(&mut tx, stored_event).await? {
                outcome.inserted = outcome.inserted.saturating_add(1);
            } else {
                outcome.already_stored = outcome.already_stored.saturating_add(1);
            }
        }

        sqlx::query(
            "INSERT INTO ingest_checkpoints (stream, cursor_toid, cursor_event, latest_ledger) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (stream) DO UPDATE SET \
                 cursor_toid = EXCLUDED.cursor_toid, \
                 cursor_event = EXCLUDED.cursor_event, \
                 latest_ledger = EXCLUDED.latest_ledger, \
                 updated_at = now()",
        )
        .bind(stream)
        .bind(toid(checkpoint.cursor)?)
        .bind(i64::from(checkpoint.cursor.event_index()))
        .bind(ledger(checkpoint.latest_ledger)?)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(outcome)
    }
}

fn read_checkpoint(row: &sqlx::postgres::PgRow) -> Result<Checkpoint, StoreError> {
    let cursor_toid: i64 = row.try_get("cursor_toid")?;
    let cursor_event: i64 = row.try_get("cursor_event")?;
    let latest_ledger: i32 = row.try_get("latest_ledger")?;
    let out_of_range = |what: &'static str, value: i64| StoreError::OutOfRange {
        what,
        value: value.to_string(),
    };
    let cursor = EventPosition::new(
        u64::try_from(cursor_toid).map_err(|_| out_of_range("cursor TOID", cursor_toid))?,
        u32::try_from(cursor_event).map_err(|_| out_of_range("cursor event", cursor_event))?,
    );
    let latest_ledger = u32::try_from(latest_ledger)
        .map_err(|_| out_of_range("latest ledger", i64::from(latest_ledger)))?;
    Ok(Checkpoint {
        cursor,
        latest_ledger,
    })
}

/// Stores one event. Returns `false` if it was already stored with the same
/// content.
async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    stored: &StoredEvent,
) -> Result<bool, StoreError> {
    let event = &stored.event;
    let position = event.position;
    let encode = |value: &stellar_xdr::curr::ScVal| {
        value
            .to_xdr_base64(Limits::none())
            .map_err(|err| StoreError::Encode {
                position,
                reason: err.to_string(),
            })
    };
    let topics = event
        .raw
        .topics
        .iter()
        .map(encode)
        .collect::<Result<Vec<_>, _>>()?;
    let data = encode(&event.raw.data)?;
    let name = match &stored.decoded {
        Decoded::Event(veil) => veil.name(),
        Decoded::Ignored { name } => name.as_str(),
    };

    let inserted = sqlx::query(
        "INSERT INTO contract_events \
             (toid, event_index, ledger, contract, source, name, topics_xdr, data_xdr) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (toid, event_index) DO NOTHING",
    )
    .bind(toid(position)?)
    .bind(event_index(position)?)
    .bind(ledger(position.ledger())?)
    .bind(&event.contract)
    .bind(source_name(event.source))
    .bind(name)
    .bind(&topics)
    .bind(&data)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;

    if !inserted {
        // Both sources return the XDR the contract emitted, so the same
        // position must carry the same bytes whichever source stored it.
        let row = sqlx::query(
            "SELECT contract, source, topics_xdr, data_xdr FROM contract_events \
             WHERE toid = $1 AND event_index = $2",
        )
        .bind(toid(position)?)
        .bind(event_index(position)?)
        .fetch_one(&mut **tx)
        .await?;
        let same = row.try_get::<String, _>("contract")? == event.contract
            && row.try_get::<Vec<String>, _>("topics_xdr")? == topics
            && row.try_get::<String, _>("data_xdr")? == data;
        if !same {
            return Err(StoreError::Conflict {
                position,
                stored_source: row.try_get("source")?,
                incoming_source: event.source,
            });
        }
        return Ok(false);
    }

    if let Decoded::Event(veil) = &stored.decoded {
        insert_typed(tx, event, veil).await?;
    }
    Ok(true)
}

/// Writes the typed row for an event whose raw row was just inserted.
async fn insert_typed(
    tx: &mut Transaction<'_, Postgres>,
    event: &ContractEvent,
    veil: &VeilEvent,
) -> Result<(), StoreError> {
    let position = event.position;
    let (toid, event_index, ledger) = (
        toid(position)?,
        event_index(position)?,
        ledger(position.ledger())?,
    );
    let contract = event.contract.as_str();

    match veil {
        VeilEvent::NewCommitment {
            commitment,
            index,
            encrypted_output,
        } => {
            sqlx::query(
                "INSERT INTO pool_commitments \
                     (toid, event_index, pool, leaf_index, commitment, encrypted_output, origin, ledger) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'transact', $7)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(contract)
            .bind(i64::from(*index))
            .bind(bytes(commitment))
            .bind(encrypted_output.as_slice())
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::NewNullifier { nullifier } => {
            sqlx::query(
                "INSERT INTO pool_nullifiers (toid, event_index, pool, nullifier, ledger) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(contract)
            .bind(bytes(nullifier))
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::Settlement(settlement) => {
            sqlx::query(
                "INSERT INTO pool_settlements \
                     (toid, event_index, pool, nullifier, output_commitment0, output_commitment1, \
                      output_index0, output_index1, amount_bucket, public_amount, recipient, asset, ledger) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::numeric, $10, $11, $12, $13)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(&settlement.pool)
            .bind(bytes(&settlement.nullifier))
            .bind(bytes(&settlement.output_commitment0))
            .bind(bytes(&settlement.output_commitment1))
            .bind(i64::from(settlement.output_index0))
            .bind(i64::from(settlement.output_index1))
            .bind(settlement.amount_bucket.to_string())
            .bind(bytes(&settlement.public_amount))
            .bind(settlement.recipient.as_deref())
            .bind(&settlement.asset)
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::Exit(exit) => {
            sqlx::query(
                "INSERT INTO pool_exits \
                     (toid, event_index, pool, nullifier, amount_bucket, public_amount, recipient, asset, ledger) \
                 VALUES ($1, $2, $3, $4, $5::numeric, $6, $7, $8, $9)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(&exit.pool)
            .bind(bytes(&exit.nullifier))
            .bind(exit.amount_bucket.to_string())
            .bind(bytes(&exit.public_amount))
            .bind(&exit.recipient)
            .bind(&exit.asset)
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::LegacyDeposit(deposit) => {
            sqlx::query(
                "INSERT INTO pool_commitments \
                     (toid, event_index, pool, leaf_index, commitment, encrypted_output, origin, ledger) \
                 VALUES ($1, $2, $3, $4, $5, NULL, 'legacy_deposit', $6)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(&deposit.pool)
            .bind(i64::from(deposit.index))
            .bind(bytes(&deposit.commitment))
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO pool_legacy_deposits \
                     (toid, event_index, pool, leaf_index, amount_bucket, asset, ledger) \
                 VALUES ($1, $2, $3, $4, $5::numeric, $6, $7)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(&deposit.pool)
            .bind(i64::from(deposit.index))
            .bind(deposit.amount_bucket.to_string())
            .bind(&deposit.asset)
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::PublicKey {
            owner,
            encryption_key,
            note_key,
        } => {
            sqlx::query(
                "INSERT INTO pool_public_keys \
                     (toid, event_index, pool, owner, encryption_key, note_key, ledger) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(contract)
            .bind(owner)
            .bind(encryption_key.as_slice())
            .bind(note_key.as_slice())
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::LeafAdded { leaf, index, root } => {
            let leaf_index = i64::try_from(*index).map_err(|_| StoreError::OutOfRange {
                what: "allowlist leaf index",
                value: index.to_string(),
            })?;
            sqlx::query(
                "INSERT INTO allowlist_events \
                     (toid, event_index, allowlist, kind, leaf, leaf_index, root, ledger) \
                 VALUES ($1, $2, $3, 'added', $4, $5, $6, $7)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(contract)
            .bind(bytes(leaf))
            .bind(leaf_index)
            .bind(bytes(root))
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
        VeilEvent::LeafRevoked { leaf, root } => {
            sqlx::query(
                "INSERT INTO allowlist_events \
                     (toid, event_index, allowlist, kind, leaf, leaf_index, root, ledger) \
                 VALUES ($1, $2, $3, 'revoked', $4, NULL, $5, $6)",
            )
            .bind(toid)
            .bind(event_index)
            .bind(contract)
            .bind(bytes(leaf))
            .bind(bytes(root))
            .bind(ledger)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

fn bytes(value: &U256Be) -> &[u8] {
    value.as_bytes()
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::StellarExpert => "stellar_expert",
        Source::Rpc => "rpc",
    }
}

fn toid(position: EventPosition) -> Result<i64, StoreError> {
    i64::try_from(position.toid()).map_err(|_| StoreError::OutOfRange {
        what: "TOID",
        value: position.toid().to_string(),
    })
}

fn event_index(position: EventPosition) -> Result<i32, StoreError> {
    i32::try_from(position.event_index()).map_err(|_| StoreError::OutOfRange {
        what: "event index",
        value: position.event_index().to_string(),
    })
}

fn ledger(sequence: u32) -> Result<i32, StoreError> {
    i32::try_from(sequence).map_err(|_| StoreError::OutOfRange {
        what: "ledger",
        value: sequence.to_string(),
    })
}
