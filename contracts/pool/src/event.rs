//! Contract events emitted by the privacy pool.
//!
//! All events are indexer-safe: they expose only public metadata needed to
//! reconstruct commitment ordering and spend status without leaking senders,
//! plaintext notes, or proof internals.

use soroban_sdk::{Address, Bytes, U256, contractevent};

/// Event emitted when a new commitment is added to the Merkle tree
///
/// This event allows off-chain observers to track new UTXOs and decrypt
/// outputs intended for them.
#[contractevent]
#[derive(Clone)]
pub struct NewCommitmentEvent {
    /// The commitment hash added to the tree
    #[topic]
    pub commitment: U256,
    /// Index position in the Merkle tree
    pub index: u32,
    /// Encrypted output data (decryptable by the recipient)
    pub encrypted_output: Bytes,
}

/// Event emitted when a nullifier is spent
///
/// This event allows off-chain observers to track which UTXOs have been spent.
#[contractevent]
#[derive(Clone)]
pub struct NewNullifierEvent {
    /// The nullifier that was spent
    #[topic]
    pub nullifier: U256,
}

/// Event emitted after a shielded transaction settles
///
/// The event carries indexer-safe public metadata that lets off-chain services
/// track spend status, output ordering, and public withdrawal details without
/// inspecting proofs, plaintext notes, senders, or encrypted output payloads.
#[contractevent(topics = ["Settlement"])]
#[derive(Clone)]
pub struct SettlementEvent {
    /// Spent nullifier hash. One event is emitted for each input nullifier.
    #[topic]
    pub nullifier: U256,
    /// Pool identifier for indexers consuming multiple pool contracts
    #[topic]
    pub pool: Address,
    /// First output commitment inserted by this transaction
    pub output_commitment0: U256,
    /// Second output commitment inserted by this transaction
    pub output_commitment1: U256,
    /// Merkle tree index for the first output commitment
    pub output_index0: u32,
    /// Merkle tree index for the second output commitment
    pub output_index1: u32,
    /// Public amount bucket associated with the external amount
    pub amount_bucket: i128,
    /// Public amount passed to the verifier, encoded in the BN254 field
    pub public_amount: U256,
    /// Public withdrawal recipient. `None` for private sends.
    pub recipient: Option<Address>,
    /// Token contract settled by the pool
    pub asset: Address,
}

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
