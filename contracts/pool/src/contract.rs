#![allow(clippy::too_many_arguments)]
use crate::{
    error::ContractError,
    event::{DepositEvent, PublicKeyEvent},
    merkle_with_history::MerkleTreeWithHistory,
    storage,
    storage_types::{DataKey, MAX_FEE_BPS},
    types::{Account, PoolConfig},
};
use soroban_sdk::{
    Address, BytesN, Env, Map, U256, contract, contractclient, contractimpl, token::TokenClient,
};
use soroban_utils::constants::bn256_modulus;

// Contract clients for cross-contract dependencies
#[contractclient(crate_path = "soroban_sdk", name = "ASPMembershipClient")]
pub trait ASPMembershipInterface {
    fn get_root(env: Env) -> Result<U256, soroban_sdk::Error>;
}

/// Privacy Pool Contract
///
/// Implements a private transaction pool.
/// Users can deposit tokens, perform private transfers, and withdraw while
/// maintaining transaction privacy through zero-knowledge proofs.
#[contract]
pub struct PoolContract;

#[contractimpl]
impl PoolContract {
    /// Constructor: initialize the privacy pool contract
    ///
    /// Sets up the contract with the specified token, verifier, and Merkle tree
    /// configuration. This function can only be called once.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `admin` - Address of the contract administrator
    /// * `token` - Address of the token contract for deposits/withdrawals
    /// * `verifier` - Address of the ZK proof verifier contract
    /// * `asp_membership` - Address of the ASP Membership contract
    /// * `maximum_deposit_amount` - Maximum allowed deposit per transaction
    /// * `fee_recipient` - Address that receives protocol fees
    /// * `fee_bps` - Protocol fee in basis points (0-10_000)
    /// * `levels` - Number of levels in the commitment Merkle tree (1-32)
    ///
    /// The MVP keeps paused defaulted to false at deploy time.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if already initialized or
    /// invalid configuration
    pub fn __constructor(
        env: Env,
        admin: Address,
        token: Address,
        verifier: Address,
        asp_membership: Address,
        maximum_deposit_amount: U256,
        fee_recipient: Address,
        fee_bps: u32,
        levels: u32,
    ) -> Result<(), ContractError> {
        if fee_bps > MAX_FEE_BPS {
            return Err(ContractError::InvalidFee);
        }

        storage::set_admin(&env, &admin);
        storage::set_token(&env, &token);
        storage::set_verifier(&env, &verifier);
        storage::set_asp_membership(&env, &asp_membership);
        storage::set_maximum_deposit(&env, &maximum_deposit_amount);
        storage::set_fee_recipient(&env, &fee_recipient);
        storage::set_fee_bps(&env, fee_bps);
        storage::set_paused(&env, false);
        storage::set_nullifiers(&env, &Map::<U256, bool>::new(&env));
        storage::set_commitments(&env, &Map::<U256, bool>::new(&env));

        // Initialize the Merkle tree for commitment storage
        MerkleTreeWithHistory::init(&env, levels)?;

        Ok(())
    }

    // ======================================================================
    // Client entry points (state-changing), ordered by lifecycle:
    //   register -> deposit
    // ======================================================================

    /// Register a user's public encryption key
    ///
    /// Allows users to publish their public key so others can send them
    /// encrypted outputs for private transfers.
    /// The account owner must authorize this call
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `account` - Account data containing owner address and public key
    pub fn register(env: Env, account: Account) {
        account.owner.require_auth();
        PublicKeyEvent {
            owner: account.owner,
            encryption_key: account.encryption_key,
            note_key: account.note_key,
        }
        .publish(&env);
    }

    /// Deposit public tokens and insert one private commitment into the pool
    ///
    /// This lightweight deposit path is used by the MVP UI before full proof
    /// orchestration is wired in. The commitment is inserted as the left leaf
    /// of a two-leaf batch, paired with a zero placeholder.
    pub fn deposit(
        env: &Env,
        from: Address,
        amount: i128,
        commitment: U256,
    ) -> Result<u32, ContractError> {
        from.require_auth();

        if amount <= 0 {
            return Err(ContractError::WrongExtAmount);
        }
        let deposit_amount = U256::from_u128(
            env,
            u128::try_from(amount).map_err(|_| ContractError::WrongExtAmount)?,
        );
        if deposit_amount > storage::get_maximum_deposit(env)? {
            return Err(ContractError::WrongExtAmount);
        }
        Self::ensure_field_element(env, &commitment)?;
        Self::ensure_commitment_unused(env, &commitment)?;

        let token = storage::get_token(env)?;
        let token_client = TokenClient::new(env, &token);
        let this = env.current_contract_address();
        token_client.transfer(&from, &this, &amount);

        let zero = U256::from_u32(env, 0);
        let (commitment_index, _) =
            MerkleTreeWithHistory::insert_two_leaves(env, commitment.clone(), zero)?;
        Self::mark_commitment_inserted(env, &commitment)?;

        DepositEvent {
            commitment,
            pool: this,
            index: commitment_index,
            amount_bucket: amount,
            asset: token,
        }
        .publish(env);

        Ok(commitment_index)
    }

    // ======================================================================
    // Read-only views
    // ======================================================================

    /// Get the latest root of the Merkle tree that defines the pool
    pub fn get_root(env: &Env) -> Result<U256, ContractError> {
        Ok(MerkleTreeWithHistory::get_last_root(env)?)
    }

    /// Get deterministic public pool configuration
    pub fn get_config(env: &Env) -> Result<PoolConfig, ContractError> {
        Ok(PoolConfig {
            admin: storage::get_admin(env)?,
            token: storage::get_token(env)?,
            verifier: storage::get_verifier(env)?,
            asp_membership: storage::get_asp_membership(env)?,
            maximum_deposit_amount: storage::get_maximum_deposit(env)?,
            fee_recipient: storage::get_fee_recipient(env)?,
            fee_bps: storage::get_fee_bps(env)?,
            paused: storage::is_paused(env)?,
        })
    }

    /// Return whether a nullifier has already been spent.
    ///
    /// Clients can use this view for diagnostics and indexer reconciliation.
    /// Spending still happens only through shielded transactions.
    pub fn has_nullifier(env: &Env, nullifier: U256) -> Result<bool, ContractError> {
        Self::is_spent(env, &nullifier)
    }

    /// Return whether a commitment has already been inserted into the pool.
    ///
    /// `deposit` returns the stable Merkle leaf index for the inserted
    /// commitment. This lightweight view lets clients and tests check the
    /// duplicate guard without reading raw contract storage.
    pub fn has_commitment(env: &Env, commitment: U256) -> Result<bool, ContractError> {
        Self::is_commitment_inserted(env, &commitment)
    }

    /// Get the current Merkle root from the ASP Membership contract
    ///
    /// Makes a cross-contract call to retrieve the current root of the
    /// membership Merkle tree.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    ///
    /// The current membership Merkle root as U256
    pub fn get_asp_membership_root(env: &Env) -> Result<U256, ContractError> {
        let asp_address = storage::get_asp_membership(env)?;
        let client = ASPMembershipClient::new(env, &asp_address);
        Ok(client.get_root())
    }

    // ======================================================================
    // Admin / configuration
    // ======================================================================

    /// Update the contract administrator
    ///
    /// Transfers administrative control to a new address. Requires
    /// authorization from the current admin.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `new_admin` - New address that will have administrative permissions
    pub fn update_admin(env: Env, new_admin: Address) -> Result<(), ContractError> {
        if !storage::has_admin(&env) {
            return Err(ContractError::NotInitialized);
        }
        soroban_utils::update_admin(&env, &DataKey::Admin, &new_admin);
        Ok(())
    }

    /// Update the ZK proof verifier contract address
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `new_verifier` - New verifier contract address
    pub fn update_verifier(env: &Env, new_verifier: Address) -> Result<(), ContractError> {
        let admin = storage::get_admin(env)?;
        admin.require_auth();
        storage::set_verifier(env, &new_verifier);
        Ok(())
    }

    /// Update the ASP Membership contract address
    ///
    /// Changes the ASP Membership contract address. Requires admin
    /// authorization.
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `new_asp_membership` - New ASP Membership contract address
    pub fn update_asp_membership(
        env: &Env,
        new_asp_membership: Address,
    ) -> Result<(), ContractError> {
        let admin = storage::get_admin(env)?;
        admin.require_auth();
        storage::set_asp_membership(env, &new_asp_membership);
        Ok(())
    }

    /// Upgrade the pool contract WASM in place
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `new_wasm_hash` - Hash of the already-uploaded replacement WASM
    pub fn upgrade(env: &Env, new_wasm_hash: BytesN<32>) -> Result<(), ContractError> {
        let admin = storage::get_admin(env)?;
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    // ======================================================================
    // Internal helpers
    // ======================================================================

    fn ensure_field_element(env: &Env, value: &U256) -> Result<(), ContractError> {
        if value.clone() >= bn256_modulus(env) {
            return Err(ContractError::InvalidFieldElement);
        }
        Ok(())
    }

    /// Check if a nullifier has already been spent
    ///
    /// # Arguments
    ///
    /// * `env` - The Soroban environment
    /// * `n` - The nullifier to check
    ///
    /// # Returns
    ///
    /// Returns `true` if the nullifier has been spent, `false` otherwise
    fn is_spent(env: &Env, n: &U256) -> Result<bool, ContractError> {
        let nulls = storage::get_nullifiers(env)?;
        Ok(nulls.get(n.clone()).unwrap_or(false))
    }

    /// Check if a commitment has already been inserted
    fn is_commitment_inserted(env: &Env, commitment: &U256) -> Result<bool, ContractError> {
        let commitments = storage::get_commitments(env)?;
        Ok(commitments.get(commitment.clone()).unwrap_or(false))
    }

    /// Ensure a commitment can be inserted exactly once
    fn ensure_commitment_unused(env: &Env, commitment: &U256) -> Result<(), ContractError> {
        if Self::is_commitment_inserted(env, commitment)? {
            return Err(ContractError::AlreadyInsertedCommitment);
        }
        Ok(())
    }

    /// Mark a commitment as inserted
    fn mark_commitment_inserted(env: &Env, commitment: &U256) -> Result<(), ContractError> {
        let mut commitments = storage::get_commitments(env)?;
        commitments.set(commitment.clone(), true);
        storage::set_commitments(env, &commitments);
        Ok(())
    }
}
