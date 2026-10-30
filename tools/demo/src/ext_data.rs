//! The pool's `ExtData` and the hash a proof binds it with.
//!
//! The pool hashes `ExtData` as `keccak256(to_xdr(ext_data)) mod p`
//! (`contracts/pool/src/contract.rs`, `hash_ext_data`). A `#[contracttype]`
//! struct is an `ScVal::Map` keyed by field name, sorted, so this module
//! builds that map with `stellar-xdr` and hashes it the same way. The tests
//! compare it with the contract's own function and with a value the deployed
//! pool returned.

use std::str::FromStr;

use num_bigint::BigUint;
use sha3::{Digest, Keccak256};
use stellar_xdr::curr::{
    Int256Parts, Limits, ScAddress, ScBytes, ScMap, ScMapEntry, ScSymbol, ScVal, WriteXdr,
};

/// BN254 scalar field modulus.
const MODULUS: &str =
    "21888242871839275222246405745257275088548364400416034343698204186575808495617";

/// The pool's external data for one `transact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtData {
    /// Strkey of the address paid on a withdrawal. Unused otherwise.
    pub recipient: String,
    /// Positive moves value in, negative out.
    pub ext_amount: i128,
    pub encrypted_output0: Vec<u8>,
    pub encrypted_output1: Vec<u8>,
}

fn i256(value: i128) -> ScVal {
    // Sign-extend the 128-bit value to 256 bits.
    let high: i64 = if value < 0 { -1 } else { 0 };
    let bytes = value.to_be_bytes();
    let mut hi = [0u8; 8];
    let mut lo = [0u8; 8];
    hi.copy_from_slice(&bytes[..8]);
    lo.copy_from_slice(&bytes[8..]);
    ScVal::I256(Int256Parts {
        hi_hi: high,
        hi_lo: u64::from_ne_bytes(high.to_ne_bytes()),
        lo_hi: u64::from_be_bytes(hi),
        lo_lo: u64::from_be_bytes(lo),
    })
}

fn entry(key: &str, val: ScVal) -> anyhow::Result<ScMapEntry> {
    Ok(ScMapEntry {
        key: ScVal::Symbol(
            ScSymbol::try_from(key).map_err(|()| anyhow::anyhow!("bad symbol {key}"))?,
        ),
        val,
    })
}

fn bytes(value: &[u8]) -> anyhow::Result<ScVal> {
    Ok(ScVal::Bytes(ScBytes(value.to_vec().try_into()?)))
}

impl ExtData {
    /// The `ScVal` the contract sees, fields in sorted order.
    pub fn to_scval(&self) -> anyhow::Result<ScVal> {
        let recipient = ScAddress::from_str(&self.recipient)
            .map_err(|_| anyhow::anyhow!("{} is not an address", self.recipient))?;
        let entries = vec![
            entry("encrypted_output0", bytes(&self.encrypted_output0)?)?,
            entry("encrypted_output1", bytes(&self.encrypted_output1)?)?,
            entry("ext_amount", i256(self.ext_amount))?,
            entry("recipient", ScVal::Address(recipient))?,
        ];
        Ok(ScVal::Map(Some(ScMap(entries.try_into()?))))
    }

    /// `keccak256(xdr) mod p`, as 32 big-endian bytes.
    pub fn hash(&self) -> anyhow::Result<[u8; 32]> {
        let xdr = self.to_scval()?.to_xdr(Limits::none())?;
        let digest = Keccak256::digest(&xdr);
        let modulus = BigUint::from_str(MODULUS)?;
        let reduced = BigUint::from_bytes_be(&digest).modpow(&BigUint::from(1u8), &modulus);
        let raw = reduced.to_bytes_be();
        let mut out = [0u8; 32];
        for (slot, byte) in out.iter_mut().rev().zip(raw.iter().rev()) {
            *slot = *byte;
        }
        Ok(out)
    }

    /// The argument `stellar contract invoke` takes for this struct.
    pub fn to_cli_json(&self) -> serde_json::Value {
        serde_json::json!({
            "recipient": self.recipient,
            "ext_amount": self.ext_amount.to_string(),
            "encrypted_output0": hex::encode(&self.encrypted_output0),
            "encrypted_output1": hex::encode(&self.encrypted_output1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deployed pool's `get_ext_data_hash`, simulated on testnet on
    /// 2026-10-30, for this exact `ExtData`.
    #[test]
    fn the_hash_matches_the_deployed_pool() {
        let ext = ExtData {
            recipient: "GAL7JBSJMXTCCAZUDPG3IHWDCHLV6AQJFOIH6QU7NF4PGEWDGOWBDDFO".to_owned(),
            ext_amount: 1_000_000,
            encrypted_output0: vec![1, 2],
            encrypted_output1: Vec::new(),
        };
        assert_eq!(
            hex::encode(ext.hash().expect("hashes")),
            "0295dcc4fe58688cf82de66742b85a4ff9be8d1f1c8b16803653527911e166dc"
        );
    }

    #[test]
    fn negative_amounts_are_sign_extended() {
        let ScVal::I256(parts) = i256(-5) else {
            panic!("not an i256");
        };
        assert_eq!(parts.hi_hi, -1);
        assert_eq!(parts.hi_lo, u64::MAX);
        assert_eq!(parts.lo_hi, u64::MAX);
        assert_eq!(parts.lo_lo, u64::MAX - 4);
    }
}
