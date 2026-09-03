#![no_std]

//! Groth16 verifier contract for Circom proofs on Soroban using the native
//! BN254 precompile.
//!
//! The verification key is embedded at compile time via `build.rs`. By default
//! the contract uses this repo's 2-input/2-output policy transaction key from
//! `circuits/keys/policy_tx_2_2_vk.json`. Set the `VERIFIER_VK_JSON`
//! environment variable to override it:
//!
//! ```bash
//! VERIFIER_VK_JSON=/path/to/other_verification_key.json \
//!   cargo build -p circom-groth16-verifier --release
//! ```

// Use Soroban's allocator for heap allocations
extern crate alloc;

pub use contract_types::{Groth16Error, Groth16Proof, VerificationKeyBytes};

pub mod contract;
mod verification_key;

pub use contract::*;
pub use verification_key::VerificationKey;

// Re-exported for the test module (`use super::*`) which builds a key from the
// embedded bytes and calls `verify_with_vk` directly.
#[cfg(test)]
pub(crate) use verification_key::verification_key_from_bytes;

#[cfg(test)]
mod test;
