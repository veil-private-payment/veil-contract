//! Contract events emitted by the ASP membership Merkle tree.

use soroban_sdk::{U256, contractevent};

/// Event emitted when a new leaf is added to the Merkle tree
#[contractevent(topics = ["LeafAdded"])]
#[derive(Clone)]
pub struct LeafAddedEvent {
    /// The leaf value that was inserted
    pub leaf: U256,
    /// Index position where the leaf was inserted
    pub index: u64,
    /// New Merkle root after insertion
    pub root: U256,
}

/// Event emitted when a leaf is revoked and the tree is rebuilt off chain
///
/// The pool checks a spend against the allowlist's current root and keeps no
/// history of it, so a published root takes effect on the next transaction: a
/// revoked key stops being able to spend at once.
#[contractevent(topics = ["LeafRevoked"])]
#[derive(Clone)]
pub struct LeafRevokedEvent {
    /// The leaf that is no longer a member
    pub leaf: U256,
    /// Root of the rebuilt tree, as published by the admin
    pub root: U256,
}
