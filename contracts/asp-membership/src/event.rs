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
