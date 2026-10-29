//! Response bodies.
//!
//! Field elements (commitments, nullifiers, roots, leaves) are 32-byte
//! big-endian values written as `0x` and 64 hex digits. Other byte strings
//! (notes, keys) are `0x` and hex of any length. Addresses are strkeys.

use serde::Serialize;
use utoipa::ToSchema;

/// Where the index stands.
#[derive(Debug, Serialize, ToSchema)]
pub struct Status {
    pub network_passphrase: String,
    pub pool: String,
    pub allowlist: String,
    /// Last ledger the indexer has fully read. Every answer reflects the
    /// chain up to this ledger.
    pub indexed_ledger: Option<u32>,
    /// Newest ledger RPC reported when the indexer last wrote.
    pub latest_ledger: Option<u32>,
    /// When the indexer last advanced (RFC 3339).
    pub updated_at: Option<String>,
}

/// The tree state the indexer last read from a contract, and whether the
/// index matched it.
#[derive(Debug, Serialize, ToSchema)]
pub struct OnChain {
    pub ledger: u32,
    pub levels: u32,
    pub next_index: u64,
    pub root: String,
    /// The index's own computation matched this state when it was read.
    pub matches: bool,
    /// RFC 3339.
    pub checked_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PoolState {
    pub contract: String,
    pub levels: u32,
    /// Number of tree positions filled, the index of the next leaf.
    pub next_index: u64,
    /// Root computed from the indexed commitments.
    pub root: String,
    pub indexed_ledger: Option<u32>,
    /// What the pool held on chain at the last verification. Equal roots at
    /// equal sizes mean the index holds the pool's data.
    pub on_chain: Option<OnChain>,
}

/// One position of the pool's tree.
#[derive(Debug, Serialize, ToSchema)]
pub struct Leaf {
    pub leaf_index: u64,
    pub commitment: String,
    /// `transact`, `legacy_deposit`, or `legacy_deposit_zero`: the zero leaf
    /// the closed deposit path inserted beside each deposit without an event.
    pub origin: String,
    /// Ledger the leaf was inserted in.
    pub ledger: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LeafPage {
    pub items: Vec<Leaf>,
    /// Pass as `cursor` for the next page. Absent on the last page.
    pub next_cursor: Option<u64>,
    pub indexed_ledger: Option<u32>,
}

/// A Merkle proof in the form the circuit takes it.
#[derive(Debug, Serialize, ToSchema)]
pub struct MerklePath {
    pub leaf_index: u64,
    pub leaf: String,
    /// Siblings from the leaf up, one per level.
    pub path_elements: Vec<String>,
    /// Bit `i` set when the node at level `i` is a right child.
    pub path_indices: u64,
    pub root: String,
    pub indexed_ledger: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NullifierStatus {
    pub nullifier: String,
    pub spent: bool,
    /// Ledger of the spend, when spent.
    pub ledger: Option<u32>,
    /// Event position of the spend (`<toid>-<index>`), when spent.
    pub event: Option<String>,
    pub indexed_ledger: Option<u32>,
}

/// An encrypted note, for recipients to try to decrypt
/// (see `docs/NOTE_ENCRYPTION.md`).
#[derive(Debug, Serialize, ToSchema)]
pub struct Note {
    pub leaf_index: u64,
    pub commitment: String,
    /// The bytes the sender published. May be empty.
    pub encrypted_output: String,
    pub ledger: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NotePage {
    pub items: Vec<Note>,
    /// Pass as `cursor` for the next page. Absent on the last page.
    pub next_cursor: Option<u64>,
    pub indexed_ledger: Option<u32>,
}

/// The keys an account published to receive payments.
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicKey {
    pub owner: String,
    /// X25519 key notes are encrypted to.
    pub encryption_key: String,
    /// BN254 key hashed into commitments.
    pub note_key: String,
    pub ledger: u32,
    /// How many times the owner has registered. The latest wins.
    pub registrations: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AllowlistEntry {
    /// `added` or `revoked`.
    pub kind: String,
    pub leaf: String,
    /// Set for additions.
    pub leaf_index: Option<u64>,
    /// Root the contract emitted with this change.
    pub root: String,
    pub ledger: u32,
    /// Event position (`<toid>-<index>`). Pass as `cursor` to page.
    pub event: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AllowlistPage {
    pub items: Vec<AllowlistEntry>,
    /// Pass as `cursor` for the next page. Absent on the last page.
    pub next_cursor: Option<String>,
    pub indexed_ledger: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AllowlistState {
    pub contract: String,
    pub levels: u32,
    /// After a revocation the operator rebuilds the tree off chain and
    /// publishes roots. Paths can then not be derived from events.
    pub operator_maintained: bool,
    /// Additions recorded on chain.
    pub additions: u64,
    pub revocations: u64,
    /// Latest root the contract emitted.
    pub root: Option<String>,
    pub indexed_ledger: Option<u32>,
    pub on_chain: Option<OnChain>,
}
