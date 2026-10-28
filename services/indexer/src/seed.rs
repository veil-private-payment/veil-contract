//! The one-time seed from Stellar Expert and the hand-off to RPC.
//!
//! The seed reads each contract's full history from Stellar Expert. Before
//! reading, it records the newest ledger Stellar Expert has processed: every
//! event at or before that ledger is in the listing, so once both contracts
//! are read, the `stellar_expert` checkpoint says "complete up to here".
//!
//! RPC then starts a margin before that ledger. The events in the overlap are
//! read from both sources, and the store checks each such pair is byte for
//! byte the same event, so the hand-off doubles as a cross-check of the two
//! sources. If RPC no longer retains the ledger the seed ended at, there would
//! be a hole between the two, and start-up refuses instead.

use crate::chain::LedgerRange;
use crate::event::Decoded;
use crate::expert::{ExpertClient, ExpertError};
use crate::ingest::{Contracts, IngestError, RPC_STREAM};
use crate::position::EventPosition;
use crate::source::SourceError;
use crate::store::{Checkpoint, Store, StoreError, StoredEvent};

/// Checkpoint stream marking the seed complete up to its ledger.
pub const SEED_STREAM: &str = "stellar_expert";

/// Ledgers re-read from RPC before the seed's last ledger, about a day.
pub const DEFAULT_OVERLAP: u32 = 17_280;

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error(transparent)]
    Expert(#[from] ExpertError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Ingest(#[from] IngestError),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("Stellar Expert returned {contract} event {event} at or before {previous}")]
    OutOfOrder {
        contract: String,
        previous: String,
        event: String,
    },
    #[error("Stellar Expert returned an event of {found} when asked for {asked}")]
    WrongContract { asked: String, found: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    pub pages: usize,
    pub inserted: usize,
    pub already_stored: usize,
    /// Ledger the seed is complete up to.
    pub complete_to: u32,
}

/// The end-of-ledger cursor: every event of `ledger` sorts at or before it.
pub fn end_of_ledger(ledger: u32) -> EventPosition {
    EventPosition::new((u64::from(ledger) << 32) | u64::from(u32::MAX), u32::MAX)
}

/// Reads both contracts' history from Stellar Expert into the store.
/// Resumable: each contract's progress is checkpointed page by page.
pub async fn seed(
    expert: &ExpertClient,
    store: &Store,
    contracts: &Contracts,
    page_size: u32,
) -> Result<SeedReport, SeedError> {
    let tip = expert.latest_ledger().await?;
    let mut report = SeedReport {
        complete_to: tip,
        ..SeedReport::default()
    };

    for contract in [&contracts.pool, &contracts.allowlist] {
        let stream = format!("{SEED_STREAM}:{contract}");
        let mut previous = store.checkpoint(&stream).await?.map(|c| c.cursor);
        loop {
            let cursor = previous.map(expert_id);
            let records = expert
                .events(contract, cursor.as_deref(), page_size)
                .await?;
            report.pages = report.pages.saturating_add(1);
            let full = u32::try_from(records.len()).map_or(true, |len| len >= page_size);

            let mut batch = Vec::with_capacity(records.len());
            for record in records {
                let event = record.into_event()?;
                if &event.contract != contract {
                    return Err(SeedError::WrongContract {
                        asked: contract.clone(),
                        found: event.contract,
                    });
                }
                if previous.is_some_and(|previous| event.position <= previous) {
                    return Err(SeedError::OutOfOrder {
                        contract: contract.clone(),
                        previous: previous.map(|p| p.to_string()).unwrap_or_default(),
                        event: event.position.to_string(),
                    });
                }
                previous = Some(event.position);
                let decoded = event.raw.decode().map_err(|source| IngestError::Decode {
                    position: event.position.to_string(),
                    source,
                })?;
                contracts.check(&event, &decoded)?;
                if let Decoded::Ignored { name } = &decoded {
                    tracing::info!(position = %event.position, name, "skipping host event");
                }
                batch.push(StoredEvent { event, decoded });
            }

            if let Some(cursor) = previous
                && !batch.is_empty()
            {
                let outcome = store
                    .write_batch(
                        &stream,
                        &batch,
                        Checkpoint {
                            cursor,
                            latest_ledger: tip,
                        },
                    )
                    .await?;
                report.inserted = report.inserted.saturating_add(outcome.inserted);
                report.already_stored =
                    report.already_stored.saturating_add(outcome.already_stored);
            }
            if !full {
                break;
            }
        }
        tracing::info!(contract, "seeded from Stellar Expert");
    }

    store
        .write_batch(
            SEED_STREAM,
            &[],
            Checkpoint {
                cursor: end_of_ledger(tip),
                latest_ledger: tip,
            },
        )
        .await?;
    Ok(report)
}

/// Stellar Expert's id format for a position: unpadded TOID, four-digit
/// event index.
fn expert_id(position: EventPosition) -> String {
    format!("{:018}-{:04}", position.toid(), position.event_index())
}

/// Why RPC ingestion cannot start or continue without losing events.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("no RPC checkpoint and no completed seed: run `veil-indexer seed` first")]
    NotSeeded,
    #[error(
        "gap: events are stored up to ledger {stored}, but RPC only retains \
         ledgers {oldest} and later. Run `veil-indexer seed` again to fill it."
    )]
    Gap { stored: u32, oldest: u32 },
}

/// The ledger RPC ingestion should start from, or `None` to resume from the
/// stored RPC checkpoint. Fails rather than skip ledgers RPC no longer has.
pub async fn rpc_start(
    store: &Store,
    range: LedgerRange,
    overlap: u32,
) -> Result<Option<u32>, HandoffError> {
    if let Some(checkpoint) = store.checkpoint(RPC_STREAM).await? {
        let stored = checkpoint.cursor.ledger();
        // The cursor's own ledger is complete only when it is an end-of-scan
        // cursor, so resuming needs the ledger after it at the latest.
        if stored.saturating_add(1) < range.oldest_ledger {
            return Err(HandoffError::Gap {
                stored,
                oldest: range.oldest_ledger,
            });
        }
        return Ok(None);
    }
    let seed = store
        .checkpoint(SEED_STREAM)
        .await?
        .ok_or(HandoffError::NotSeeded)?;
    let complete_to = seed.latest_ledger;
    if complete_to.saturating_add(1) < range.oldest_ledger {
        return Err(HandoffError::Gap {
            stored: complete_to,
            oldest: range.oldest_ledger,
        });
    }
    Ok(Some(
        complete_to.saturating_sub(overlap).max(range.oldest_ledger),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expert_ids_round_trip() {
        let position: EventPosition = "020013091605499905-0001".parse().expect("parses");
        assert_eq!(expert_id(position), "020013091605499905-0001");
    }

    #[test]
    fn the_end_of_a_ledger_sorts_after_its_events() {
        let event: EventPosition = "020013091605499905-0001".parse().expect("parses");
        assert!(end_of_ledger(event.ledger()) > event);
        assert!(
            end_of_ledger(event.ledger())
                < EventPosition::new(u64::from(event.ledger().saturating_add(1)) << 32, 0)
        );
    }
}
