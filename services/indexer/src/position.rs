//! Where an event sits in the ledger history.
//!
//! Soroban RPC and Stellar Expert both identify a contract event by the TOID
//! of the operation that emitted it plus the event's index within that
//! operation. They format it differently (RPC pads both halves with zeros,
//! Stellar Expert does not), but the two numbers are the same, so parsing both
//! into one [`EventPosition`] is what lets events from either source be
//! deduplicated against each other.

use core::fmt;
use core::str::FromStr;

/// Number of low bits of a TOID that hold the transaction and operation
/// order. The ledger sequence sits above them.
const TOID_LEDGER_SHIFT: u32 = 32;

/// Position of a contract event: the emitting operation's TOID and the event's
/// index within that operation.
///
/// Ordering follows ledger history, so sorting positions sorts events in the
/// order the contract emitted them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventPosition {
    toid: u64,
    event_index: u32,
}

impl EventPosition {
    pub const fn new(toid: u64, event_index: u32) -> Self {
        Self { toid, event_index }
    }

    /// TOID of the operation that emitted the event.
    pub const fn toid(self) -> u64 {
        self.toid
    }

    /// Index of the event within its operation.
    pub const fn event_index(self) -> u32 {
        self.event_index
    }

    /// Ledger sequence the event was emitted in.
    pub fn ledger(self) -> u32 {
        let ledger = self.toid.checked_shr(TOID_LEDGER_SHIFT).unwrap_or(0);
        // A TOID is a signed 64-bit value with the ledger in the top 32 bits,
        // so the shifted value always fits.
        u32::try_from(ledger).unwrap_or(u32::MAX)
    }
}

impl fmt::Display for EventPosition {
    /// Formats the position the way Soroban RPC does, which is the format the
    /// RPC accepts back as a cursor.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:019}-{:010}", self.toid, self.event_index)
    }
}

/// Why an event id could not be read as a position.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("event id {id:?} is not `<toid>-<event index>`")]
pub struct ParsePositionError {
    id: String,
}

impl FromStr for EventPosition {
    type Err = ParsePositionError;

    /// Accepts both `0020013091605499905-0000000001` (Soroban RPC) and
    /// `020013091605499905-0001` (Stellar Expert).
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let error = || ParsePositionError { id: id.to_owned() };
        let (toid, index) = id.split_once('-').ok_or_else(error)?;
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits(toid) || !digits(index) {
            return Err(error());
        }
        let toid = toid.parse::<u64>().map_err(|_| error())?;
        let event_index = index.parse::<u32>().map_err(|_| error())?;
        Ok(Self::new(toid, event_index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_id_formats_name_the_same_position() {
        let rpc: EventPosition = "0020013091605499905-0000000001"
            .parse()
            .expect("rpc id parses");
        let expert: EventPosition = "020013091605499905-0001".parse().expect("expert id parses");
        assert_eq!(rpc, expert);
        assert_eq!(rpc.toid(), 20_013_091_605_499_905);
        assert_eq!(rpc.event_index(), 1);
    }

    #[test]
    fn the_ledger_is_the_top_half_of_the_toid() {
        let position: EventPosition = "020013091605499905-0001".parse().expect("parses");
        assert_eq!(position.ledger(), 4_659_661);
    }

    #[test]
    fn display_round_trips_in_rpc_format() {
        let position = EventPosition::new(20_013_091_605_499_905, 1);
        assert_eq!(position.to_string(), "0020013091605499905-0000000001");
        assert_eq!(position.to_string().parse::<EventPosition>(), Ok(position));
    }

    #[test]
    fn positions_sort_in_emission_order() {
        let first = EventPosition::new(10, 1);
        let second = EventPosition::new(10, 2);
        let third = EventPosition::new(11, 0);
        assert!(first < second && second < third);
    }

    #[test]
    fn malformed_ids_are_refused() {
        for id in [
            "", "123", "-1", "1-", "abc-1", "1-x", "1-2-3", "+1-1", "1--1",
        ] {
            assert!(
                id.parse::<EventPosition>().is_err(),
                "{id:?} should be refused"
            );
        }
    }
}
