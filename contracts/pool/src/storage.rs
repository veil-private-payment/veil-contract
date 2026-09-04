//! Persistent storage accessors for the privacy pool.
//!
//! Every read/write against contract storage funnels through these helpers so
//! `contract.rs` stays focused on business logic and never touches `DataKey`
//! directly. Getters return [`ContractError::NotInitialized`] when the contract
//! has not been constructed yet.

use soroban_sdk::{Address, Env, Map, U256};

use crate::error::ContractError;
use crate::storage_types::DataKey;

// ========== Nullifiers map ==========

/// Get the nullifiers map from storage
pub(crate) fn get_nullifiers(env: &Env) -> Result<Map<U256, bool>, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Nullifiers)
        .ok_or(ContractError::NotInitialized)
}

/// Save the nullifiers map to storage
pub(crate) fn set_nullifiers(env: &Env, m: &Map<U256, bool>) {
    env.storage().persistent().set(&DataKey::Nullifiers, m);
}

// ========== Commitments map ==========

/// Get the inserted commitments map from storage
pub(crate) fn get_commitments(env: &Env) -> Result<Map<U256, bool>, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Commitments)
        .ok_or(ContractError::NotInitialized)
}

/// Save the inserted commitments map to storage
pub(crate) fn set_commitments(env: &Env, m: &Map<U256, bool>) {
    env.storage().persistent().set(&DataKey::Commitments, m);
}

// ========== Token ==========

/// Get the token contract address
pub(crate) fn get_token(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Token)
        .ok_or(ContractError::NotInitialized)
}

/// Save the token contract address
pub(crate) fn set_token(env: &Env, token: &Address) {
    env.storage().persistent().set(&DataKey::Token, token);
}

// ========== Maximum deposit ==========

/// Get the maximum deposit amount
pub(crate) fn get_maximum_deposit(env: &Env) -> Result<U256, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::MaximumDepositAmount)
        .ok_or(ContractError::NotInitialized)
}

/// Save the maximum deposit amount
pub(crate) fn set_maximum_deposit(env: &Env, amount: &U256) {
    env.storage()
        .persistent()
        .set(&DataKey::MaximumDepositAmount, amount);
}

// ========== Verifier ==========

/// Get the verifier contract address
pub(crate) fn get_verifier(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Verifier)
        .ok_or(ContractError::NotInitialized)
}

/// Save the verifier contract address
pub(crate) fn set_verifier(env: &Env, verifier: &Address) {
    env.storage().persistent().set(&DataKey::Verifier, verifier);
}

// ========== Fee recipient ==========

/// Get the protocol fee recipient address
pub(crate) fn get_fee_recipient(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::FeeRecipient)
        .ok_or(ContractError::NotInitialized)
}

/// Save the protocol fee recipient address
pub(crate) fn set_fee_recipient(env: &Env, recipient: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::FeeRecipient, recipient);
}

// ========== Fee bps ==========

/// Get the protocol fee in basis points
pub(crate) fn get_fee_bps(env: &Env) -> Result<u32, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::FeeBps)
        .ok_or(ContractError::NotInitialized)
}

/// Save the protocol fee in basis points
pub(crate) fn set_fee_bps(env: &Env, fee_bps: u32) {
    env.storage().persistent().set(&DataKey::FeeBps, &fee_bps);
}

// ========== Admin ==========

/// Return whether an admin address has been stored
pub(crate) fn has_admin(env: &Env) -> bool {
    env.storage().persistent().has(&DataKey::Admin)
}

/// Get the admin address
pub(crate) fn get_admin(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::Admin)
        .ok_or(ContractError::NotInitialized)
}

/// Save the admin address
pub(crate) fn set_admin(env: &Env, admin: &Address) {
    env.storage().persistent().set(&DataKey::Admin, admin);
}

// ========== ASP contracts ==========

/// Get the ASP Membership contract address
pub(crate) fn get_asp_membership(env: &Env) -> Result<Address, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::ASPMembership)
        .ok_or(ContractError::NotInitialized)
}

/// Save the ASP Membership contract address
pub(crate) fn set_asp_membership(env: &Env, address: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::ASPMembership, address);
}
