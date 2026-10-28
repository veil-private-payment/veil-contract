//! Reads events from a source page by page and stores them.
//!
//! Each page is decoded completely before anything is written, then stored
//! together with the cursor that follows it in one transaction. A restart
//! therefore resumes from the last page that committed, re-reading at most the
//! page that was in flight, and that re-read stores nothing new.

use crate::event::{Decoded, Emitter, VeilEvent};
use crate::rpc::{EventSource, RpcError, Start};
use crate::source::ContractEvent;
use crate::store::{Checkpoint, Store, StoreError, StoredEvent};

/// Checkpoint stream for events read from Soroban RPC.
pub const RPC_STREAM: &str = "rpc";

/// The two contracts the indexer follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contracts {
    pub pool: String,
    pub allowlist: String,
}

impl Contracts {
    fn ids(&self) -> Vec<String> {
        vec![self.pool.clone(), self.allowlist.clone()]
    }

    /// Checks that an event came from the contract that emits its kind, and
    /// that a pool event naming its pool names the one being followed.
    pub fn check(&self, event: &ContractEvent, decoded: &Decoded) -> Result<(), IngestError> {
        let Decoded::Event(veil) = decoded else {
            return Ok(());
        };
        let expected = match veil.emitter() {
            Emitter::Pool => &self.pool,
            Emitter::Allowlist => &self.allowlist,
        };
        let named_pool = match veil {
            VeilEvent::Settlement(settlement) => Some(&settlement.pool),
            VeilEvent::Exit(exit) => Some(&exit.pool),
            VeilEvent::LegacyDeposit(deposit) => Some(&deposit.pool),
            _ => None,
        };
        if &event.contract != expected || named_pool.is_some_and(|pool| pool != &self.pool) {
            return Err(IngestError::WrongContract {
                position: event.position.to_string(),
                event: veil.name(),
                contract: event.contract.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("event {position}: {source}")]
    Decode {
        position: String,
        #[source]
        source: crate::event::DecodeError,
    },
    #[error("event {position} ({event}) came from {contract}, which does not emit it")]
    WrongContract {
        position: String,
        event: &'static str,
        contract: String,
    },
    #[error("source returned event {event} at or before the cursor {cursor}")]
    OutOfOrder { cursor: String, event: String },
    #[error("source moved the cursor back from {from} to {to}")]
    CursorRegression { from: String, to: String },
    #[error("stream {stream} has no checkpoint and no start ledger was given")]
    NoStart { stream: String },
}

/// Totals for one [`Follower::sync_to_tip`] run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pages: usize,
    pub inserted: usize,
    pub already_stored: usize,
    pub ignored: usize,
    pub checkpoint: Option<Checkpoint>,
}

pub struct Follower<'a, S> {
    pub source: &'a S,
    pub store: &'a Store,
    pub contracts: &'a Contracts,
    pub stream: &'a str,
    pub page_size: u32,
}

impl<S: EventSource> Follower<'_, S> {
    /// Reads from the stream's checkpoint, or from `start_ledger` if it has
    /// none, until the source reports it is at the tip.
    pub async fn sync_to_tip(&self, start_ledger: Option<u32>) -> Result<SyncReport, IngestError> {
        let mut report = SyncReport::default();
        let mut start = match self.store.checkpoint(self.stream).await? {
            Some(checkpoint) => {
                report.checkpoint = Some(checkpoint);
                Start::After(checkpoint.cursor)
            }
            None => Start::Ledger(start_ledger.ok_or_else(|| IngestError::NoStart {
                stream: self.stream.to_owned(),
            })?),
        };
        let ids = self.contracts.ids();

        loop {
            let page = self.source.events(start, &ids, self.page_size).await?;
            report.pages = report.pages.saturating_add(1);
            // A full page may stop short inside the latest ledger.
            let full = u32::try_from(page.events.len()).map_or(true, |len| len >= self.page_size);
            let at_tip = page.at_tip() && !full;

            let mut batch = Vec::with_capacity(page.events.len());
            let mut previous = match start {
                Start::After(cursor) => Some(cursor),
                Start::Ledger(_) => None,
            };
            for event in page.events {
                if let Some(previous) = previous
                    && event.position <= previous
                {
                    return Err(IngestError::OutOfOrder {
                        cursor: previous.to_string(),
                        event: event.position.to_string(),
                    });
                }
                previous = Some(event.position);
                let decoded = event.raw.decode().map_err(|source| IngestError::Decode {
                    position: event.position.to_string(),
                    source,
                })?;
                self.contracts.check(&event, &decoded)?;
                if let Decoded::Ignored { name } = &decoded {
                    tracing::info!(position = %event.position, name, "skipping host event");
                    report.ignored = report.ignored.saturating_add(1);
                }
                batch.push(StoredEvent { event, decoded });
            }
            if let Some(last) = previous
                && page.cursor < last
            {
                return Err(IngestError::CursorRegression {
                    from: last.to_string(),
                    to: page.cursor.to_string(),
                });
            }

            let checkpoint = Checkpoint {
                cursor: page.cursor,
                latest_ledger: page.latest_ledger,
            };
            let unchanged = start == Start::After(page.cursor);
            if !unchanged || !batch.is_empty() {
                let outcome = self
                    .store
                    .write_batch(self.stream, &batch, checkpoint)
                    .await?;
                report.inserted = report.inserted.saturating_add(outcome.inserted);
                report.already_stored =
                    report.already_stored.saturating_add(outcome.already_stored);
                report.checkpoint = Some(checkpoint);
                tracing::debug!(
                    stream = self.stream,
                    cursor = %page.cursor,
                    inserted = outcome.inserted,
                    already_stored = outcome.already_stored,
                    "stored page"
                );
            }

            if at_tip || unchanged {
                return Ok(report);
            }
            start = Start::After(page.cursor);
        }
    }
}
