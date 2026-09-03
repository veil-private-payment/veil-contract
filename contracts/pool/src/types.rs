use contract_types::Groth16Proof;
use soroban_sdk::{Address, Bytes, BytesN, I256, U256, Vec, contracttype};

/// Zero-knowledge proof data for a transaction
///
/// Contains all the cryptographic data needed to verify a transaction,
/// including the proof itself, public inputs, and nullifiers.
#[contracttype]
pub struct Proof {
    /// The serialized zero-knowledge proof
    pub proof: Groth16Proof,
    /// Merkle root the proof was generated against
    pub root: U256,
    /// Nullifiers for spent input UTXOs (prevents double-spending)
    pub input_nullifiers: Vec<U256>,
    /// Commitment for the first output UTXO
    pub output_commitment0: U256,
    /// Commitment for the second output UTXO
    pub output_commitment1: U256,
    /// Net public amount (deposit - withdrawal, modulo field size)
    pub public_amount: U256,
    /// Hash of the external data (binds proof to transaction parameters)
    pub ext_data_hash: BytesN<32>,
    /// Merkle root the ASP membership proof was generated against
    pub asp_membership_root: U256,
}

/// External data for a transaction
///
/// Contains public information about the transaction that is hashed and
/// included in the zero-knowledge proof to bind the proof to specific
/// transaction parameters (e.g. recipient address).
#[contracttype]
#[derive(Clone)]
pub struct ExtData {
    /// Recipient address for withdrawals
    pub recipient: Address,
    /// External amount: positive for deposits, negative for withdrawals
    pub ext_amount: I256,
    /// Encrypted data for the first output UTXO
    pub encrypted_output0: Bytes,
    /// Encrypted data for the second output UTXO
    pub encrypted_output1: Bytes,
}

/// User account registration data
///
/// Used for registering a user's public key to enable encrypted communication
/// for receiving transfers.
/// Not required to interact with the pool. But facilitates in-pool transfers
/// via events. As parties can learn about each other public key.
#[contracttype]
pub struct Account {
    /// Owner address of the account
    pub owner: Address,
    /// X25519 encryption public key for encrypting note data (32 bytes)
    pub encryption_key: Bytes,
    /// BN254 note public key for creating commitments (32 bytes)
    pub note_key: Bytes,
}

/// Public pool configuration.
///
/// This view is intentionally limited to deterministic contract configuration
/// that clients and tests can safely read without inspecting raw storage.
#[contracttype]
pub struct PoolConfig {
    /// Administrator address with permissions to modify contract settings
    pub admin: Address,
    /// Address of the token contract used for deposits/withdrawals
    pub token: Address,
    /// Address of the ZK proof verifier contract
    pub verifier: Address,
    /// Address of the ASP Membership contract
    pub asp_membership: Address,
    /// Maximum allowed deposit amount per transaction
    pub maximum_deposit_amount: U256,
    /// Address that receives protocol fees
    pub fee_recipient: Address,
    /// Protocol fee in basis points
    pub fee_bps: u32,
    /// Whether state-changing pool operations are paused
    pub paused: bool,
}
