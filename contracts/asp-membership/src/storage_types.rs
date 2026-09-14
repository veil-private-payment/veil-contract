//! Storage keys for the ASP membership Merkle tree.
//!
//! Keeping the `DataKey` enum in one place mirrors the pool contract layout and
//! lets `storage.rs` and `contract.rs` share a single source of truth for the
//! persistent storage layout.

use soroban_sdk::contracttype;

/// Storage keys for contract persistent data
#[contracttype]
#[derive(Clone, Debug)]
pub(crate) enum DataKey {
    /// Administrator address with permissions to modify the tree
    Admin,
    /// Filled subtree hashes at each level (indexed by level)
    FilledSubtrees(u32),
    /// Zero hash values for each level (indexed by level)
    Zeroes(u32),
    /// Number of levels in the Merkle tree
    Levels,
    /// Next available index for leaf insertion
    NextIndex,
    /// Current Merkle root
    Root,
    /// Whether admin permission is required to insert a leaf
    AdminInsertOnly,
    /// Set once a leaf has been revoked, after which the tree is rebuilt off
    /// chain and the admin publishes each new root.
    OperatorMaintained,
}
