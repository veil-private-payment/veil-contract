//! Storage keys and storage-related constants for the privacy pool.
//!
//! Keeping the `DataKey` enum and tunable constants in one place mirrors the
//! Phoenix/Blend layout and lets `storage.rs` and `contract.rs` share a single
//! source of truth for persistent storage layout.

use soroban_sdk::contracttype;

/// Maximum protocol fee expressed in basis points (100% = 10_000 bps).
pub(crate) const MAX_FEE_BPS: u32 = 10_000;

/// Storage keys for contract persistent data
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DataKey {
    /// Administrator address with permissions to modify contract settings
    Admin,
    /// Address of the token contract used for deposits/withdrawals
    Token,
    /// Address of the ZK proof verifier contract
    Verifier,
    /// Maximum allowed deposit amount per transaction
    MaximumDepositAmount,
    /// Map of spent nullifiers (nullifier -> bool)
    Nullifiers,
    /// Map of inserted commitments (commitment -> bool)
    Commitments,
    /// Address of the ASP Membership contract
    ASPMembership,
    /// Address that receives protocol fees
    FeeRecipient,
    /// Protocol fee in basis points
    FeeBps,
}
