#![allow(clippy::too_many_arguments)]
use crate::{
    error::ContractError,
    event::{DepositEvent, NewCommitmentEvent, NewNullifierEvent, PublicKeyEvent, SettlementEvent},
    merkle_with_history::MerkleTreeWithHistory,
    storage,
    storage_types::{DataKey, MAX_FEE_BPS},
    types::{Account, ExtData, PoolConfig, Proof},
    verifier_boundary,
};
use soroban_sdk::{
    Address, Bytes, BytesN, Env, I256, Map, U256, Vec, contract, contractclient, contractimpl,
    token::TokenClient, xdr::ToXdr,
};
use soroban_utils::constants::bn256_modulus;

/// Hash external data using Keccak256
///
/// Serializes the external data to XDR, hashes it with Keccak256,
/// and reduces the result modulo the BN256 field size.
pub fn hash_ext_data(env: &Env, ext: &ExtData) -> BytesN<32> {
    let payload = ext.clone().to_xdr(env);
    let digest: BytesN<32> = env.crypto().keccak256(&payload).into();
    let digest_u256 = U256::from_be_bytes(env, &Bytes::from(digest));
    let reduced = digest_u256.rem_euclid(&bn256_modulus(env));
    let mut buf = [0u8; 32];
    reduced.to_be_bytes().copy_into_slice(&mut buf);
    BytesN::from_array(env, &buf)
}

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

    /// Execute a shielded transaction with deposit handling
    ///
    /// This is the main entry point for users to interact with the pool.
    /// If `ext_amount > 0`, tokens are transferred from the sender to the pool
    /// before processing the transaction.
    pub fn transact(
        env: &Env,
        proof: Proof,
        ext_data: ExtData,
        sender: Address,
    ) -> Result<(), ContractError> {
        sender.require_auth();
        let token = storage::get_token(env)?;
        let token_client = TokenClient::new(env, &token);
        let zero = I256::from_i32(env, 0);

        // Handle deposit if ext_amount > 0
        if ext_data.ext_amount > zero {
            let deposit_u = U256::from_be_bytes(env, &ext_data.ext_amount.to_be_bytes());
            let max = storage::get_maximum_deposit(env)?;
            if deposit_u > max {
                return Err(ContractError::WrongExtAmount);
            }
            let this = env.current_contract_address();
            let amount = Self::i256_to_i128_nonneg(env, &ext_data.ext_amount)?;
            token_client.transfer(&sender, &this, &amount);
        }

        Self::internal_transact(env, proof, ext_data)
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

    /// Return the canonical external-data hash used by `transact`.
    ///
    /// Off-chain proof adapters and local fixtures can call this view to verify
    /// they are binding the exact same public withdrawal/send data as the pool.
    pub fn get_ext_data_hash(env: &Env, ext_data: ExtData) -> BytesN<32> {
        Self::hash_ext_data(env, &ext_data)
    }

    /// Return the field-encoded public amount `transact` expects for a given
    /// external amount.
    ///
    /// Deposits encode directly; withdrawals encode as `FIELD_SIZE - amount`.
    /// Clients call this view so the public amount in the proof matches what
    /// the pool will compute from `ExtData`.
    pub fn get_public_amount(env: &Env, ext_amount: I256) -> Result<U256, ContractError> {
        Self::calculate_public_amount(env, ext_amount)
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

    // ======================================================================
    // Internal helpers - the shielded-transaction pipeline and its checks.
    // `transact` funnels into `internal_transact`, which runs validation
    // steps from cheapest to most expensive (field check -> root -> nullifier
    // -> ext hash -> ASP root -> ZK verify) before mutating state.
    // ======================================================================

    /// Process a private transaction
    ///
    /// Validates the proof and all public inputs, marks nullifiers as spent,
    /// processes withdrawals, and inserts new commitments into the Merkle tree.
    fn internal_transact(env: &Env, proof: Proof, ext_data: ExtData) -> Result<(), ContractError> {
        Self::ensure_proof_field_elements(env, &proof)?;

        // 1. Merkle root check
        if !MerkleTreeWithHistory::is_known_root(env, &proof.root)? {
            return Err(ContractError::UnknownRoot);
        }
        // 2. Nullifier checks (prevent double-spending)
        Self::ensure_nullifiers_unspent(env, &proof.input_nullifiers)?;
        // 3. External data hash check
        let ext_hash = Self::hash_ext_data(env, &ext_data);
        if ext_hash != proof.ext_data_hash {
            return Err(ContractError::WrongExtHash);
        }

        // 4. Public amount check
        let expected_public_amount =
            Self::calculate_public_amount(env, ext_data.ext_amount.clone())?;
        if proof.public_amount != expected_public_amount {
            return Err(ContractError::WrongExtAmount);
        }

        // 5. ASP membership root must match the live allowlist. The circuit
        //    binds the spender key to this root, so a spender who is not
        //    enrolled cannot produce a proof that satisfies both.
        let member_root = Self::get_asp_membership_root(env)?;
        if member_root != proof.asp_membership_root {
            return Err(ContractError::InvalidProof);
        }

        // 6. ZK proof verification
        if !Self::verify_proof(env, &proof)? {
            return Err(ContractError::InvalidProof);
        }

        Self::ensure_commitment_unused(env, &proof.output_commitment0)?;
        Self::ensure_commitment_unused(env, &proof.output_commitment1)?;
        if proof.output_commitment0 == proof.output_commitment1 {
            return Err(ContractError::AlreadyInsertedCommitment);
        }

        // 7. Mark nullifiers as spent
        Self::spend_nullifiers_once(env, &proof.input_nullifiers)?;
        for n in proof.input_nullifiers.iter() {
            NewNullifierEvent { nullifier: n }.publish(env);
        }

        // 8. Process withdrawal if ext_amount < 0
        let token = storage::get_token(env)?;
        let token_client = TokenClient::new(env, &token);
        let this = env.current_contract_address();
        let zero = I256::from_i32(env, 0);

        let withdrawal_recipient = if ext_data.ext_amount < zero {
            let abs = zero.sub(&ext_data.ext_amount);
            let amount: i128 = Self::i256_to_i128_nonneg(env, &abs)?;
            let fee = Self::calculate_fee_amount(amount, storage::get_fee_bps(env)?)?;
            let recipient_amount = amount.checked_sub(fee).ok_or(ContractError::Overflow)?;

            if recipient_amount > 0 {
                token_client.transfer(&this, &ext_data.recipient, &recipient_amount);
            }
            if fee > 0 {
                let fee_recipient = storage::get_fee_recipient(env)?;
                token_client.transfer(&this, &fee_recipient, &fee);
            }
            Some(ext_data.recipient.clone())
        } else {
            None
        };

        // 9. Insert new commitments into Merkle tree
        let (idx_0, idx_1) = MerkleTreeWithHistory::insert_two_leaves(
            env,
            proof.output_commitment0.clone(),
            proof.output_commitment1.clone(),
        )?;
        Self::mark_commitment_inserted(env, &proof.output_commitment0)?;
        Self::mark_commitment_inserted(env, &proof.output_commitment1)?;

        // 10. Emit commitment events
        NewCommitmentEvent {
            commitment: proof.output_commitment0.clone(),
            index: idx_0,
            encrypted_output: ext_data.encrypted_output0.clone(),
        }
        .publish(env);

        NewCommitmentEvent {
            commitment: proof.output_commitment1.clone(),
            index: idx_1,
            encrypted_output: ext_data.encrypted_output1.clone(),
        }
        .publish(env);

        let amount_bucket = ext_data
            .ext_amount
            .to_i128()
            .ok_or(ContractError::WrongExtAmount)?;

        // 11. Emit one settlement event per nullifier for indexer lookups.
        for nullifier in proof.input_nullifiers.iter() {
            SettlementEvent {
                nullifier,
                pool: this.clone(),
                output_commitment0: proof.output_commitment0.clone(),
                output_commitment1: proof.output_commitment1.clone(),
                output_index0: idx_0,
                output_index1: idx_1,
                amount_bucket,
                public_amount: proof.public_amount.clone(),
                recipient: withdrawal_recipient.clone(),
                asset: token.clone(),
            }
            .publish(env);
        }

        Ok(())
    }

    /// Verify a zero-knowledge proof through the configured verifier contract
    fn verify_proof(env: &Env, proof: &Proof) -> Result<bool, ContractError> {
        if proof.proof.is_empty() {
            return Err(ContractError::InvalidProof);
        }
        let verifier = storage::get_verifier(env)?;
        Ok(verifier_boundary::verify_policy_transaction(
            env, &verifier, proof,
        ))
    }

    fn ensure_proof_field_elements(env: &Env, proof: &Proof) -> Result<(), ContractError> {
        Self::ensure_field_element(env, &proof.root)?;
        Self::ensure_field_element(env, &proof.public_amount)?;
        Self::ensure_field_elements(env, &proof.input_nullifiers)?;
        Self::ensure_field_element(env, &proof.output_commitment0)?;
        Self::ensure_field_element(env, &proof.output_commitment1)?;
        Self::ensure_field_element(env, &proof.asp_membership_root)?;
        Ok(())
    }

    fn ensure_field_elements(env: &Env, values: &Vec<U256>) -> Result<(), ContractError> {
        for value in values.iter() {
            Self::ensure_field_element(env, &value)?;
        }
        Ok(())
    }

    /// Ensure all provided nullifiers are currently unspent and unique.
    ///
    /// This catches both nullifiers already stored from earlier transactions
    /// and duplicate nullifiers inside the same transaction payload.
    fn ensure_nullifiers_unspent(env: &Env, nullifiers: &Vec<U256>) -> Result<(), ContractError> {
        let stored = storage::get_nullifiers(env)?;
        let mut seen: Map<U256, bool> = Map::new(env);

        for nullifier in nullifiers.iter() {
            if stored.get(nullifier.clone()).unwrap_or(false)
                || seen.get(nullifier.clone()).unwrap_or(false)
            {
                return Err(ContractError::AlreadySpentNullifier);
            }
            seen.set(nullifier, true);
        }

        Ok(())
    }

    /// Check and mark nullifiers in one storage update.
    fn spend_nullifiers_once(env: &Env, nullifiers: &Vec<U256>) -> Result<(), ContractError> {
        Self::ensure_nullifiers_unspent(env, nullifiers)?;

        let mut stored = storage::get_nullifiers(env)?;
        for nullifier in nullifiers.iter() {
            stored.set(nullifier, true);
        }
        storage::set_nullifiers(env, &stored);

        Ok(())
    }

    /// Calculate the public amount from external amount
    ///
    /// Computes `public_amount = ext_amount` in the BN256 field.
    /// For positive results, returns the value directly.
    /// For negative results, returns `FIELD_SIZE - |public_amount|`.
    fn calculate_public_amount(env: &Env, ext_amount: I256) -> Result<U256, ContractError> {
        let abs_ext = Self::i256_abs_to_u256(env, &ext_amount);
        if abs_ext >= Self::max_ext_amount(env) {
            return Err(ContractError::WrongExtAmount);
        }

        let zero = I256::from_i32(env, 0);

        if ext_amount >= zero {
            let pa_bytes = ext_amount.to_be_bytes();
            Ok(U256::from_be_bytes(env, &pa_bytes))
        } else {
            let neg = zero.sub(&ext_amount);
            let neg_bytes = neg.to_be_bytes();
            let neg_u256 = U256::from_be_bytes(env, &neg_bytes);

            let field = bn256_modulus(env);
            Ok(field.sub(&neg_u256))
        }
    }

    /// Calculate protocol fee for a public withdrawal amount.
    ///
    /// `amount` is the gross amount leaving the pool. The fee is rounded down
    /// in token base units so small withdrawals remain possible.
    fn calculate_fee_amount(amount: i128, fee_bps: u32) -> Result<i128, ContractError> {
        amount
            .checked_mul(i128::from(fee_bps))
            .and_then(|v| v.checked_div(i128::from(MAX_FEE_BPS)))
            .ok_or(ContractError::Overflow)
    }

    /// Maximum absolute external amount allowed (2^248)
    fn max_ext_amount(env: &Env) -> U256 {
        U256::from_parts(env, 0x0100_0000_0000_0000, 0, 0, 0)
    }

    /// Convert a non-negative I256 to i128 with bounds checking
    fn i256_to_i128_nonneg(env: &Env, v: &I256) -> Result<i128, ContractError> {
        if *v < I256::from_i32(env, 0) {
            return Err(ContractError::WrongExtAmount);
        }
        v.to_i128().ok_or(ContractError::WrongExtAmount)
    }

    /// Convert I256 to its absolute value as U256
    fn i256_abs_to_u256(env: &Env, v: &I256) -> U256 {
        let zero = I256::from_i32(env, 0);
        let abs = if *v >= zero { v.clone() } else { zero.sub(v) };
        U256::from_be_bytes(env, &abs.to_be_bytes())
    }

    fn hash_ext_data(env: &Env, ext: &ExtData) -> BytesN<32> {
        hash_ext_data(env, ext)
    }
}
