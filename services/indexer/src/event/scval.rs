//! Typed readers over the `ScVal`s a contract event carries.
//!
//! Every reader names the event and field it is reading, so a decode failure
//! says exactly which part of which event changed shape.

use std::collections::BTreeMap;

use stellar_xdr::curr::{ScAddress, ScMap, ScVal};

use super::{DecodeError, U256Be};

/// The fields of an event body, keyed by name.
///
/// Fields are taken one at a time and [`Fields::finish`] refuses any that were
/// not taken, so a contract that adds a field to an event is noticed instead
/// of silently ignored.
pub(super) struct Fields<'a> {
    event: &'static str,
    entries: BTreeMap<&'a str, &'a ScVal>,
}

impl<'a> Fields<'a> {
    pub(super) fn new(event: &'static str, data: &'a ScVal) -> Result<Self, DecodeError> {
        let ScVal::Map(Some(ScMap(entries))) = data else {
            return Err(DecodeError::shape(event, "body", "a map of fields"));
        };
        let mut fields = BTreeMap::new();
        for entry in entries.iter() {
            let ScVal::Symbol(key) = &entry.key else {
                return Err(DecodeError::shape(event, "body", "symbol field names"));
            };
            let key = core::str::from_utf8(key.as_slice())
                .ok()
                .ok_or_else(|| DecodeError::shape(event, "body", "UTF-8 field names"))?;
            if fields.insert(key, &entry.val).is_some() {
                return Err(DecodeError::shape(event, "body", "unique field names"));
            }
        }
        Ok(Self {
            event,
            entries: fields,
        })
    }

    pub(super) fn take(&mut self, field: &'static str) -> Result<&'a ScVal, DecodeError> {
        self.entries.remove(field).ok_or(DecodeError::MissingField {
            event: self.event,
            field,
        })
    }

    pub(super) fn finish(self) -> Result<(), DecodeError> {
        match self.entries.keys().next() {
            None => Ok(()),
            Some(field) => Err(DecodeError::UnexpectedField {
                event: self.event,
                field: (*field).to_owned(),
            }),
        }
    }
}

/// Reads the symbol in the first topic, which names the event.
pub(super) fn event_name(topics: &[ScVal]) -> Result<&str, DecodeError> {
    match topics.first() {
        Some(ScVal::Symbol(name)) => core::str::from_utf8(name.as_slice())
            .ok()
            .ok_or_else(|| DecodeError::shape("event", "topic 0", "a UTF-8 symbol")),
        _ => Err(DecodeError::shape(
            "event",
            "topic 0",
            "a symbol naming the event",
        )),
    }
}

/// Returns the topics after the name, checking there are exactly `N`.
pub(super) fn topics<'a, const N: usize>(
    event: &'static str,
    topics: &'a [ScVal],
) -> Result<&'a [ScVal; N], DecodeError> {
    let rest = topics.get(1..).unwrap_or_default();
    <&[ScVal; N]>::try_from(rest).map_err(|_| DecodeError::TopicCount {
        event,
        expected: N,
        found: rest.len(),
    })
}

pub(super) fn u256(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<U256Be, DecodeError> {
    let ScVal::U256(parts) = val else {
        return Err(DecodeError::shape(event, field, "a U256"));
    };
    let mut bytes = [0u8; 32];
    let words = [parts.hi_hi, parts.hi_lo, parts.lo_hi, parts.lo_lo];
    for (chunk, word) in bytes.chunks_exact_mut(8).zip(words) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    Ok(U256Be(bytes))
}

pub(super) fn u32(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<u32, DecodeError> {
    match val {
        ScVal::U32(value) => Ok(*value),
        _ => Err(DecodeError::shape(event, field, "a u32")),
    }
}

pub(super) fn u64(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<u64, DecodeError> {
    match val {
        ScVal::U64(value) => Ok(*value),
        _ => Err(DecodeError::shape(event, field, "a u64")),
    }
}

pub(super) fn i128(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<i128, DecodeError> {
    let ScVal::I128(parts) = val else {
        return Err(DecodeError::shape(event, field, "an i128"));
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&parts.hi.to_be_bytes());
    bytes[8..].copy_from_slice(&parts.lo.to_be_bytes());
    Ok(i128::from_be_bytes(bytes))
}

pub(super) fn bytes(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<Vec<u8>, DecodeError> {
    match val {
        ScVal::Bytes(bytes) => Ok(bytes.to_vec()),
        _ => Err(DecodeError::shape(event, field, "bytes")),
    }
}

/// Reads an address as its strkey (`G…` for an account, `C…` for a contract).
pub(super) fn address(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<String, DecodeError> {
    match val {
        ScVal::Address(address @ (ScAddress::Account(_) | ScAddress::Contract(_))) => {
            Ok(address.to_string())
        }
        _ => Err(DecodeError::shape(
            event,
            field,
            "an account or contract address",
        )),
    }
}

/// Reads an `Option<Address>`, which Soroban encodes as `Void` for `None`.
pub(super) fn optional_address(
    event: &'static str,
    field: &'static str,
    val: &ScVal,
) -> Result<Option<String>, DecodeError> {
    match val {
        ScVal::Void => Ok(None),
        other => address(event, field, other).map(Some),
    }
}
