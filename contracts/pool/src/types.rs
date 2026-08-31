use soroban_sdk::{Address, Bytes, U256, contracttype};

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
