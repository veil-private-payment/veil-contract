//! Event records as the two history sources return them.
//!
//! Soroban RPC keeps about seven days of events, so a pool's older history is
//! read once from Stellar Expert, which keeps every event with the same topics
//! and body XDR. Both sources are reduced to one [`ContractEvent`] here, with
//! the source recorded, so nothing downstream depends on where an event came
//! from and a partial history cannot pass as a complete one.

use serde::{Deserialize, Serialize};

use crate::event::{DecodeError, RawEvent};
use crate::position::{EventPosition, ParsePositionError};

/// Where an event was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Stellar Expert's event archive, used once to seed history.
    StellarExpert,
    /// Soroban RPC `getEvents`, used from the seed onwards.
    Rpc,
}

/// One contract event, independent of the source it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractEvent {
    pub position: EventPosition,
    /// Strkey of the contract that emitted the event.
    pub contract: String,
    pub source: Source,
    pub raw: RawEvent,
}

/// Why a source record could not be turned into a [`ContractEvent`].
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error(transparent)]
    Position(#[from] ParsePositionError),
    #[error("event {id}: {source}")]
    Decode {
        id: String,
        #[source]
        source: DecodeError,
    },
}

/// An event record from Stellar Expert's
/// `/explorer/{network}/contract/{id}/events` endpoint.
///
/// Only the fields the indexer relies on are read. The decoded `topics` it
/// also returns are ignored in favour of the XDR, which is what the contract
/// actually emitted.
#[derive(Clone, Debug, Deserialize)]
pub struct ExpertEventRecord {
    pub id: String,
    pub contract: String,
    #[serde(rename = "topicsXdr")]
    pub topics_xdr: Vec<String>,
    #[serde(rename = "bodyXdr")]
    pub body_xdr: String,
}

impl ExpertEventRecord {
    pub fn into_event(self) -> Result<ContractEvent, SourceError> {
        let position = self.id.parse::<EventPosition>()?;
        let raw = RawEvent::from_base64(&self.topics_xdr, &self.body_xdr).map_err(|source| {
            SourceError::Decode {
                id: self.id.clone(),
                source,
            }
        })?;
        Ok(ContractEvent {
            position,
            contract: self.contract,
            source: Source::StellarExpert,
            raw,
        })
    }
}
