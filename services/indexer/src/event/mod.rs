//! Decoding of the events the pool and allowlist contracts emit.
//!
//! The pool and the allowlist publish everything an off-chain reader needs as
//! contract events. This module turns the raw `ScVal` topics and body of one
//! event into a typed [`VeilEvent`], and refuses anything whose shape differs
//! from the contract source in `contracts/pool/src/event.rs` and
//! `contracts/asp-membership/src/event.rs`: a missing field, an extra field, a
//! wrong type or a wrong number of topics is an error, never a default.

mod scval;

use core::fmt;

use serde::Serialize;

use stellar_xdr::curr::{Limits, ReadXdr, ScVal};

use scval::Fields;

/// A 256-bit value as the contracts emit it (`U256`), stored big-endian.
///
/// Commitments, nullifiers, Merkle roots, allowlist leaves and the public
/// amount are all BN254 field elements carried in this form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct U256Be(pub [u8; 32]);

impl U256Be {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for U256Be {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl Serialize for U256Be {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl fmt::Debug for U256Be {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

fn hex_bytes<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    serializer.serialize_str(&hex)
}

/// A shielded spend that settled through `transact`.
///
/// The pool emits one per input nullifier, so a two-input transaction yields
/// two settlements carrying the same outputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settlement {
    pub nullifier: U256Be,
    pub pool: String,
    pub output_commitment0: U256Be,
    pub output_commitment1: U256Be,
    pub output_index0: u32,
    pub output_index1: u32,
    pub amount_bucket: i128,
    pub public_amount: U256Be,
    /// Public withdrawal recipient, `None` for a private send.
    pub recipient: Option<String>,
    pub asset: String,
}

/// A spend out of a full pool through `exit`.
///
/// Its outputs are discarded, so it carries no commitments and must add
/// nothing to the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Exit {
    pub nullifier: U256Be,
    pub pool: String,
    pub amount_bucket: i128,
    pub public_amount: U256Be,
    pub recipient: String,
    pub asset: String,
}

/// A deposit through the closed `deposit` entrypoint.
///
/// No longer emitted, but pools deployed before the path closed have these in
/// their history, and the commitments they inserted are part of the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegacyDeposit {
    pub commitment: U256Be,
    pub pool: String,
    pub index: u32,
    pub amount_bucket: i128,
    pub asset: String,
}

/// One event from the pool or the allowlist, decoded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VeilEvent {
    /// Pool: a commitment was inserted into the tree at `index`.
    NewCommitment {
        commitment: U256Be,
        index: u32,
        #[serde(serialize_with = "hex_bytes")]
        encrypted_output: Vec<u8>,
    },
    /// Pool: a nullifier was spent.
    NewNullifier { nullifier: U256Be },
    /// Pool: a `transact` settled.
    Settlement(Settlement),
    /// Pool: an `exit` settled.
    Exit(Exit),
    /// Pool: a deposit through the closed path.
    LegacyDeposit(LegacyDeposit),
    /// Pool: an account published the keys others need to pay it.
    PublicKey {
        owner: String,
        #[serde(serialize_with = "hex_bytes")]
        encryption_key: Vec<u8>,
        #[serde(serialize_with = "hex_bytes")]
        note_key: Vec<u8>,
    },
    /// Allowlist: a leaf was inserted at `index`, giving `root`.
    LeafAdded {
        leaf: U256Be,
        index: u64,
        root: U256Be,
    },
    /// Allowlist: a leaf was revoked and the tree rebuilt to `root`.
    LeafRevoked { leaf: U256Be, root: U256Be },
}

/// Which contract emits an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Emitter {
    Pool,
    Allowlist,
}

impl VeilEvent {
    pub const fn emitter(&self) -> Emitter {
        match self {
            Self::LeafAdded { .. } | Self::LeafRevoked { .. } => Emitter::Allowlist,
            _ => Emitter::Pool,
        }
    }

    /// The event's name as it appears in the first topic.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::NewCommitment { .. } => NEW_COMMITMENT,
            Self::NewNullifier { .. } => NEW_NULLIFIER,
            Self::Settlement(_) => SETTLEMENT,
            Self::Exit(_) => EXIT,
            Self::LegacyDeposit(_) => DEPOSIT,
            Self::PublicKey { .. } => PUBLIC_KEY,
            Self::LeafAdded { .. } => LEAF_ADDED,
            Self::LeafRevoked { .. } => LEAF_REVOKED,
        }
    }
}

const NEW_COMMITMENT: &str = "new_commitment_event";
const NEW_NULLIFIER: &str = "new_nullifier_event";
const SETTLEMENT: &str = "Settlement";
const EXIT: &str = "Exit";
const DEPOSIT: &str = "Deposit";
const PUBLIC_KEY: &str = "public_key_event";
const LEAF_ADDED: &str = "LeafAdded";
const LEAF_REVOKED: &str = "LeafRevoked";

/// Events the host emits under a contract's ID that carry no Veil state.
///
/// `executable_update` is published when a contract's wasm is upgraded.
const HOST_EVENTS: &[&str] = &["executable_update"];

/// The outcome of decoding one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Event(VeilEvent),
    /// A host lifecycle event that carries no Veil state. Callers should log
    /// it and move on.
    Ignored {
        name: String,
    },
}

/// Why an event could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("event {name:?} is not one the pool or allowlist emits")]
    UnknownEvent { name: String },
    #[error("{event}: expected {expected} topics after the name, found {found}")]
    TopicCount {
        event: &'static str,
        expected: usize,
        found: usize,
    },
    #[error("{event}: field {field:?} is missing")]
    MissingField {
        event: &'static str,
        field: &'static str,
    },
    #[error("{event}: field {field:?} is not part of the contract's event")]
    UnexpectedField { event: &'static str, field: String },
    #[error("{event}: {part} should be {expected}")]
    Shape {
        event: &'static str,
        part: &'static str,
        expected: &'static str,
    },
    #[error("{part} is not valid XDR: {reason}")]
    Xdr { part: &'static str, reason: String },
}

impl DecodeError {
    fn shape(event: &'static str, part: &'static str, expected: &'static str) -> Self {
        Self::Shape {
            event,
            part,
            expected,
        }
    }
}

/// Size limits applied when reading XDR from an untrusted source.
///
/// The largest legitimate value is an encrypted note of a few hundred bytes,
/// so these leave a wide margin while still bounding what a bad response can
/// make the reader allocate.
fn xdr_limits() -> Limits {
    Limits {
        depth: 32,
        len: 65_536,
    }
}

/// Raw topics and body of one contract event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub topics: Vec<ScVal>,
    pub data: ScVal,
}

impl RawEvent {
    /// Reads an event from base64 XDR, as Soroban RPC and Stellar Expert both
    /// return it.
    pub fn from_base64<S: AsRef<str>>(topics: &[S], data: &str) -> Result<Self, DecodeError> {
        let read = |part: &'static str, b64: &str| {
            ScVal::from_xdr_base64(b64, xdr_limits()).map_err(|err| DecodeError::Xdr {
                part,
                reason: err.to_string(),
            })
        };
        let topics = topics
            .iter()
            .map(|topic| read("topic", topic.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        let data = read("body", data)?;
        Ok(Self { topics, data })
    }

    pub fn decode(&self) -> Result<Decoded, DecodeError> {
        decode(&self.topics, &self.data)
    }
}

/// Decodes one event from its topics and body.
pub fn decode(topics: &[ScVal], data: &ScVal) -> Result<Decoded, DecodeError> {
    let name = scval::event_name(topics)?;
    let event = match name {
        NEW_COMMITMENT => decode_new_commitment(topics, data)?,
        NEW_NULLIFIER => decode_new_nullifier(topics, data)?,
        SETTLEMENT => decode_settlement(topics, data)?,
        EXIT => decode_exit(topics, data)?,
        DEPOSIT => decode_deposit(topics, data)?,
        PUBLIC_KEY => decode_public_key(topics, data)?,
        LEAF_ADDED => decode_leaf_added(topics, data)?,
        LEAF_REVOKED => decode_leaf_revoked(topics, data)?,
        other if HOST_EVENTS.contains(&other) => {
            return Ok(Decoded::Ignored {
                name: other.to_owned(),
            });
        }
        other => {
            return Err(DecodeError::UnknownEvent {
                name: other.to_owned(),
            });
        }
    };
    Ok(Decoded::Event(event))
}

fn decode_new_commitment(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = NEW_COMMITMENT;
    let [commitment] = scval::topics::<1>(E, topics)?;
    let mut fields = Fields::new(E, data)?;
    let event = VeilEvent::NewCommitment {
        commitment: scval::u256(E, "commitment", commitment)?,
        index: scval::u32(E, "index", fields.take("index")?)?,
        encrypted_output: scval::bytes(E, "encrypted_output", fields.take("encrypted_output")?)?,
    };
    fields.finish()?;
    Ok(event)
}

fn decode_new_nullifier(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = NEW_NULLIFIER;
    let [nullifier] = scval::topics::<1>(E, topics)?;
    Fields::new(E, data)?.finish()?;
    Ok(VeilEvent::NewNullifier {
        nullifier: scval::u256(E, "nullifier", nullifier)?,
    })
}

fn decode_settlement(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = SETTLEMENT;
    let [nullifier, pool] = scval::topics::<2>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = Settlement {
        nullifier: scval::u256(E, "nullifier", nullifier)?,
        pool: scval::address(E, "pool", pool)?,
        output_commitment0: scval::u256(E, "output_commitment0", f.take("output_commitment0")?)?,
        output_commitment1: scval::u256(E, "output_commitment1", f.take("output_commitment1")?)?,
        output_index0: scval::u32(E, "output_index0", f.take("output_index0")?)?,
        output_index1: scval::u32(E, "output_index1", f.take("output_index1")?)?,
        amount_bucket: scval::i128(E, "amount_bucket", f.take("amount_bucket")?)?,
        public_amount: scval::u256(E, "public_amount", f.take("public_amount")?)?,
        recipient: scval::optional_address(E, "recipient", f.take("recipient")?)?,
        asset: scval::address(E, "asset", f.take("asset")?)?,
    };
    f.finish()?;
    Ok(VeilEvent::Settlement(event))
}

fn decode_exit(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = EXIT;
    let [nullifier, pool] = scval::topics::<2>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = Exit {
        nullifier: scval::u256(E, "nullifier", nullifier)?,
        pool: scval::address(E, "pool", pool)?,
        amount_bucket: scval::i128(E, "amount_bucket", f.take("amount_bucket")?)?,
        public_amount: scval::u256(E, "public_amount", f.take("public_amount")?)?,
        recipient: scval::address(E, "recipient", f.take("recipient")?)?,
        asset: scval::address(E, "asset", f.take("asset")?)?,
    };
    f.finish()?;
    Ok(VeilEvent::Exit(event))
}

fn decode_deposit(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = DEPOSIT;
    let [commitment, pool] = scval::topics::<2>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = LegacyDeposit {
        commitment: scval::u256(E, "commitment", commitment)?,
        pool: scval::address(E, "pool", pool)?,
        index: scval::u32(E, "index", f.take("index")?)?,
        amount_bucket: scval::i128(E, "amount_bucket", f.take("amount_bucket")?)?,
        asset: scval::address(E, "asset", f.take("asset")?)?,
    };
    f.finish()?;
    Ok(VeilEvent::LegacyDeposit(event))
}

fn decode_public_key(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = PUBLIC_KEY;
    let [owner] = scval::topics::<1>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = VeilEvent::PublicKey {
        owner: scval::address(E, "owner", owner)?,
        encryption_key: scval::bytes(E, "encryption_key", f.take("encryption_key")?)?,
        note_key: scval::bytes(E, "note_key", f.take("note_key")?)?,
    };
    f.finish()?;
    Ok(event)
}

fn decode_leaf_added(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = LEAF_ADDED;
    let [] = scval::topics::<0>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = VeilEvent::LeafAdded {
        leaf: scval::u256(E, "leaf", f.take("leaf")?)?,
        index: scval::u64(E, "index", f.take("index")?)?,
        root: scval::u256(E, "root", f.take("root")?)?,
    };
    f.finish()?;
    Ok(event)
}

fn decode_leaf_revoked(topics: &[ScVal], data: &ScVal) -> Result<VeilEvent, DecodeError> {
    const E: &str = LEAF_REVOKED;
    let [] = scval::topics::<0>(E, topics)?;
    let mut f = Fields::new(E, data)?;
    let event = VeilEvent::LeafRevoked {
        leaf: scval::u256(E, "leaf", f.take("leaf")?)?,
        root: scval::u256(E, "root", f.take("root")?)?,
    };
    f.finish()?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use stellar_xdr::curr::{ScMap, ScMapEntry, ScSymbol, UInt256Parts};

    use super::*;

    fn sym(name: &str) -> ScVal {
        ScVal::Symbol(ScSymbol(name.try_into().expect("short symbol")))
    }

    fn u256(lo_lo: u64) -> ScVal {
        ScVal::U256(UInt256Parts {
            hi_hi: 0,
            hi_lo: 0,
            lo_hi: 0,
            lo_lo,
        })
    }

    fn map(entries: Vec<(&str, ScVal)>) -> ScVal {
        let entries: Vec<ScMapEntry> = entries
            .into_iter()
            .map(|(key, val)| ScMapEntry { key: sym(key), val })
            .collect();
        ScVal::Map(Some(ScMap(entries.try_into().expect("small map"))))
    }

    fn leaf_added_body() -> Vec<(&'static str, ScVal)> {
        vec![
            ("index", ScVal::U64(3)),
            ("leaf", u256(1)),
            ("root", u256(2)),
        ]
    }

    #[test]
    fn a_well_formed_event_decodes() {
        let decoded = decode(&[sym(LEAF_ADDED)], &map(leaf_added_body()));
        let mut leaf = [0u8; 32];
        leaf[31] = 1;
        let mut root = [0u8; 32];
        root[31] = 2;
        assert_eq!(
            decoded,
            Ok(Decoded::Event(VeilEvent::LeafAdded {
                leaf: U256Be(leaf),
                index: 3,
                root: U256Be(root),
            }))
        );
    }

    #[test]
    fn a_missing_field_is_an_error() {
        let mut body = leaf_added_body();
        body.retain(|(key, _)| *key != "root");
        assert_eq!(
            decode(&[sym(LEAF_ADDED)], &map(body)),
            Err(DecodeError::MissingField {
                event: LEAF_ADDED,
                field: "root"
            })
        );
    }

    #[test]
    fn an_extra_field_is_an_error() {
        let mut body = leaf_added_body();
        body.push(("surprise", ScVal::U32(1)));
        assert_eq!(
            decode(&[sym(LEAF_ADDED)], &map(body)),
            Err(DecodeError::UnexpectedField {
                event: LEAF_ADDED,
                field: "surprise".to_owned()
            })
        );
    }

    #[test]
    fn a_retyped_field_is_an_error() {
        let mut body = leaf_added_body();
        body[0] = ("index", ScVal::U32(3));
        assert!(matches!(
            decode(&[sym(LEAF_ADDED)], &map(body)),
            Err(DecodeError::Shape { part: "index", .. })
        ));
    }

    #[test]
    fn a_wrong_topic_count_is_an_error() {
        assert_eq!(
            decode(&[sym(NEW_NULLIFIER)], &map(vec![])),
            Err(DecodeError::TopicCount {
                event: NEW_NULLIFIER,
                expected: 1,
                found: 0
            })
        );
    }

    #[test]
    fn an_unknown_event_is_an_error_and_a_host_event_is_ignored() {
        assert_eq!(
            decode(&[sym("transfer")], &ScVal::Void),
            Err(DecodeError::UnknownEvent {
                name: "transfer".to_owned()
            })
        );
        assert_eq!(
            decode(&[sym("executable_update")], &ScVal::Void),
            Ok(Decoded::Ignored {
                name: "executable_update".to_owned()
            })
        );
    }

    #[test]
    fn an_event_without_a_name_is_an_error() {
        assert!(decode(&[], &ScVal::Void).is_err());
        assert!(decode(&[ScVal::U32(1)], &ScVal::Void).is_err());
    }

    #[test]
    fn i128_keeps_its_sign() {
        use stellar_xdr::curr::Int128Parts;
        for value in [0i128, 1, -1, i128::MAX, i128::MIN] {
            let bytes = value.to_be_bytes();
            let parts = Int128Parts {
                hi: i64::from_be_bytes(bytes[..8].try_into().expect("8 bytes")),
                lo: u64::from_be_bytes(bytes[8..].try_into().expect("8 bytes")),
            };
            assert_eq!(scval::i128("t", "f", &ScVal::I128(parts)), Ok(value));
        }
    }

    #[test]
    fn oversized_xdr_is_refused() {
        // A Bytes value larger than the read limit.
        use stellar_xdr::curr::{ScBytes, WriteXdr};
        let big = ScVal::Bytes(ScBytes(vec![0u8; 70_000].try_into().expect("fits BytesM")));
        let b64 = big.to_xdr_base64(Limits::none()).expect("encodes");
        assert!(matches!(
            RawEvent::from_base64(std::slice::from_ref(&b64), &b64),
            Err(DecodeError::Xdr { .. })
        ));
    }
}
