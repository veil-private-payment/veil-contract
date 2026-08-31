//! Contract events emitted by the privacy pool.
//!
//! All events are indexer-safe: they expose only public metadata needed to
//! reconstruct commitment ordering and spend status without leaking senders,
//! plaintext notes, or proof internals.

use soroban_sdk::{Address, Bytes, U256, contractevent};

/// Event emitted when public tokens are deposited into the pool
///
/// The event carries only indexer-safe metadata needed to reconstruct
/// commitment ordering without exposing sender-specific data.
#[contractevent(topics = ["Deposit"])]
#[derive(Clone)]
pub struct DepositEvent {
    /// The commitment inserted into the Merkle tree
    #[topic]
    pub commitment: U256,
    /// Pool identifier for indexers consuming multiple pool contracts
    #[topic]
    pub pool: Address,
    /// Index position in the Merkle tree
    pub index: u32,
    /// Public amount bucket associated with this deposit
    pub amount_bucket: i128,
    /// Token contract accepted by the pool
    pub asset: Address,
}

/// Event emitted when a user registers their public keys
///
/// This event allows other users to discover keys for sending private
/// transfers. Two key types are required:
/// - encryption_key: X25519 key for encrypting note data (amount, blinding)
/// - note_key: BN254 key for creating commitments in the ZK circuit
#[contractevent]
#[derive(Clone)]
pub struct PublicKeyEvent {
    /// Address of the account owner
    #[topic]
    pub owner: Address,
    /// X25519 encryption public key
    pub encryption_key: Bytes,
    /// BN254 note public key
    pub note_key: Bytes,
}
