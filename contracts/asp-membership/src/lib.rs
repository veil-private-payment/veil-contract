//! ASP Membership Contract
//!
//! This contract implements a Merkle tree-based membership system using
//! Poseidon2 hash function for Anonymous Service Provider (ASP) membership
//! tracking. The contract maintains a Merkle tree where each leaf represents a
//! member, and the root serves as a commitment to the entire membership set.
#![no_std]
pub mod contract;
pub mod error;
pub mod event;
pub mod storage;
pub mod storage_types;

pub use contract::*;

// Re-exported for the test module (`use super::*`) which reads storage keys
// directly from contract storage.
#[cfg(test)]
pub(crate) use storage_types::DataKey;

#[cfg(test)]
mod test;
