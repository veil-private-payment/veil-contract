//! Contract state read straight from the ledger.
//!
//! `verify` compares what the indexer rebuilt from events with what the
//! contracts hold, so it reads their storage entries with `getLedgerEntries`
//! rather than trusting anything derived from the same events. Every entry
//! for one contract is read in a single request, so the values describe one
//! ledger.
//!
//! The keys mirror `MerkleDataKey` in `contracts/pool/src/merkle_with_history.rs`
//! and `DataKey` in `contracts/asp-membership/src/storage_types.rs`. A
//! `#[contracttype]` enum variant is stored as a vector of its name followed by
//! its fields.

use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use stellar_xdr::curr::{
    ContractDataDurability, LedgerEntryData, LedgerKey, LedgerKeyContractData, Limits, ReadXdr,
    ScAddress, ScSymbol, ScVal, ScVec, WriteXdr,
};

use crate::event::U256Be;
use crate::rpc::{RpcClient, RpcError};

/// How many roots the pool keeps (`ROOT_HISTORY_SIZE`).
pub const POOL_ROOT_HISTORY: u32 = 90;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("{0} is not a contract ID")]
    Contract(String),
    #[error("building a ledger key: {0}")]
    Key(String),
    #[error("reading a ledger entry: {0}")]
    Entry(String),
    #[error("{contract} has no {key} entry (not initialised, or archived)")]
    Missing { contract: String, key: String },
    #[error("{contract} {key} is {found}, expected {expected}")]
    Type {
        contract: String,
        key: String,
        expected: &'static str,
        found: String,
    },
}

/// The pool's tree as stored on chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PoolTree {
    pub ledger: u32,
    pub levels: u32,
    pub next_index: u64,
    pub root: U256Be,
}

/// The allowlist's tree as stored on chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AllowlistTree {
    pub ledger: u32,
    pub levels: u32,
    pub next_index: u64,
    pub root: U256Be,
    /// Set after a revocation: the admin publishes roots computed off chain.
    pub operator_maintained: bool,
}

/// The RPC ledger window, from `getHealth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRange {
    pub latest_ledger: u32,
    pub oldest_ledger: u32,
}

impl RpcClient {
    pub async fn ledger_range(&self) -> Result<LedgerRange, RpcError> {
        self.call("getHealth", &serde_json::Value::Null).await
    }

    pub async fn pool_tree(&self, pool: &str) -> Result<PoolTree, ChainError> {
        let names = pool_keys()?;
        let (entries, ledger) = self.contract_data(pool, &names).await?;
        let get = |name: &ScVal, label: &str| {
            entries.get(name).ok_or_else(|| ChainError::Missing {
                contract: pool.to_owned(),
                key: label.to_owned(),
            })
        };
        let levels = as_u32(pool, "Levels", get(&names[0], "Levels")?)?;
        let next_index = as_u64(pool, "NextIndex", get(&names[1], "NextIndex")?)?;
        let current = as_u32(
            pool,
            "CurrentRootIndex",
            get(&names[2], "CurrentRootIndex")?,
        )?;
        let label = format!("Root({current})");
        let root_key = key("Root", Some(ScVal::U32(current)))?;
        let root = as_u256(pool, &label, get(&root_key, &label)?)?;
        Ok(PoolTree {
            ledger,
            levels,
            next_index,
            root,
        })
    }

    pub async fn allowlist_tree(&self, allowlist: &str) -> Result<AllowlistTree, ChainError> {
        let names = allowlist_keys()?;
        let (entries, ledger) = self.contract_data(allowlist, &names).await?;
        let get = |name: &ScVal, label: &str| {
            entries.get(name).ok_or_else(|| ChainError::Missing {
                contract: allowlist.to_owned(),
                key: label.to_owned(),
            })
        };
        let operator_maintained = match entries.get(&names[3]) {
            None => false,
            Some(ScVal::Bool(flag)) => *flag,
            Some(other) => {
                return Err(type_error(allowlist, "OperatorMaintained", "bool", other));
            }
        };
        Ok(AllowlistTree {
            ledger,
            levels: as_u32(allowlist, "Levels", get(&names[0], "Levels")?)?,
            next_index: as_u64(allowlist, "NextIndex", get(&names[1], "NextIndex")?)?,
            root: as_u256(allowlist, "Root", get(&names[2], "Root")?)?,
            operator_maintained,
        })
    }

    /// Reads persistent entries of `contract` by storage key. Returns the
    /// values found, keyed by storage key, and the ledger they were read at.
    async fn contract_data(
        &self,
        contract: &str,
        names: &[ScVal],
    ) -> Result<(HashMap<ScVal, ScVal>, u32), ChainError> {
        let address =
            ScAddress::from_str(contract).map_err(|_| ChainError::Contract(contract.to_owned()))?;
        let keys = ledger_keys(contract, names)?;
        let result: GetLedgerEntriesResult = self
            .call("getLedgerEntries", &GetLedgerEntriesParams { keys })
            .await?;

        let mut values = HashMap::new();
        for entry in result.entries.unwrap_or_default() {
            let data = LedgerEntryData::from_xdr_base64(&entry.xdr, crate::event::xdr_limits())
                .map_err(|err| ChainError::Entry(err.to_string()))?;
            let LedgerEntryData::ContractData(data) = data else {
                return Err(ChainError::Entry("not a contract data entry".to_owned()));
            };
            if data.contract != address {
                return Err(ChainError::Entry(format!(
                    "entry belongs to {}, not {contract}",
                    data.contract
                )));
            }
            values.insert(data.key, data.val);
        }
        Ok((values, result.latest_ledger))
    }
}

#[derive(Serialize)]
struct GetLedgerEntriesParams {
    keys: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetLedgerEntriesResult {
    entries: Option<Vec<LedgerEntryResult>>,
    latest_ledger: u32,
}

#[derive(Deserialize)]
struct LedgerEntryResult {
    xdr: String,
}

/// The pool's storage keys `verify` reads: `Levels`, `NextIndex`,
/// `CurrentRootIndex`, then every `Root(i)` of the history ring.
pub fn pool_keys() -> Result<Vec<ScVal>, ChainError> {
    let mut names = vec![
        key("Levels", None)?,
        key("NextIndex", None)?,
        key("CurrentRootIndex", None)?,
    ];
    for index in 0..POOL_ROOT_HISTORY {
        names.push(key("Root", Some(ScVal::U32(index)))?);
    }
    Ok(names)
}

/// The allowlist's storage keys `verify` reads.
pub fn allowlist_keys() -> Result<Vec<ScVal>, ChainError> {
    Ok(vec![
        key("Levels", None)?,
        key("NextIndex", None)?,
        key("Root", None)?,
        key("OperatorMaintained", None)?,
    ])
}

/// Base64 `LedgerKey`s for persistent entries of `contract`.
pub fn ledger_keys(contract: &str, names: &[ScVal]) -> Result<Vec<String>, ChainError> {
    let address =
        ScAddress::from_str(contract).map_err(|_| ChainError::Contract(contract.to_owned()))?;
    names
        .iter()
        .map(|name| {
            LedgerKey::ContractData(LedgerKeyContractData {
                contract: address.clone(),
                key: name.clone(),
                durability: ContractDataDurability::Persistent,
            })
            .to_xdr_base64(Limits::none())
            .map_err(|err| ChainError::Key(err.to_string()))
        })
        .collect()
}

/// The storage key of a `#[contracttype]` enum variant.
pub fn key(variant: &str, field: Option<ScVal>) -> Result<ScVal, ChainError> {
    let symbol = ScSymbol::try_from(variant)
        .map_err(|()| ChainError::Key(format!("{variant} is not a valid symbol")))?;
    let mut parts = vec![ScVal::Symbol(symbol)];
    parts.extend(field);
    let vec = ScVec::try_from(parts).map_err(|err| ChainError::Key(err.to_string()))?;
    Ok(ScVal::Vec(Some(vec)))
}

fn type_error(contract: &str, key: &str, expected: &'static str, found: &ScVal) -> ChainError {
    ChainError::Type {
        contract: contract.to_owned(),
        key: key.to_owned(),
        expected,
        found: format!("{found:?}"),
    }
}

fn as_u32(contract: &str, key: &str, value: &ScVal) -> Result<u32, ChainError> {
    match value {
        ScVal::U32(v) => Ok(*v),
        other => Err(type_error(contract, key, "u32", other)),
    }
}

fn as_u64(contract: &str, key: &str, value: &ScVal) -> Result<u64, ChainError> {
    match value {
        ScVal::U64(v) => Ok(*v),
        other => Err(type_error(contract, key, "u64", other)),
    }
}

fn as_u256(contract: &str, key: &str, value: &ScVal) -> Result<U256Be, ChainError> {
    match value {
        ScVal::U256(parts) => {
            let mut bytes = [0u8; 32];
            for (chunk, part) in
                bytes
                    .chunks_exact_mut(8)
                    .zip([parts.hi_hi, parts.hi_lo, parts.lo_hi, parts.lo_lo])
            {
                chunk.copy_from_slice(&part.to_be_bytes());
            }
            Ok(U256Be(bytes))
        }
        other => Err(type_error(contract, key, "u256", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_keys_are_a_vector_of_name_then_fields() {
        let unit = key("Levels", None).expect("key");
        let ScVal::Vec(Some(parts)) = &unit else {
            panic!("not a vector");
        };
        assert_eq!(parts.len(), 1);
        let tuple = key("Root", Some(ScVal::U32(7))).expect("key");
        let ScVal::Vec(Some(parts)) = &tuple else {
            panic!("not a vector");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], ScVal::U32(7));
    }
}
