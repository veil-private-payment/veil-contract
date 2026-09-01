//! Persistent storage accessors for the ASP membership Merkle tree.
//!
//! Every read/write against contract storage funnels through these helpers so
//! `contract.rs` stays focused on Merkle tree logic and never touches `DataKey`
//! directly. Getters return [`Error::NotInitialized`] when the contract has not
//! been constructed yet.

use soroban_sdk::{Address, Env, U256};

use crate::error::Error;
use crate::storage_types::DataKey;

// ========== Admin ==========

/// Return whether an admin address has been stored
pub(crate) fn has_admin(env: &Env) -> bool {
    env.storage().persistent().has(&DataKey::Admin)
}

/// Get the admin address
pub(crate) fn get_admin(env: &Env) -> Result<Address, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)
}

/// Save the admin address
pub(crate) fn set_admin(env: &Env, admin: &Address) {
    env.storage().persistent().set(&DataKey::Admin, admin);
}

// ========== Levels ==========

/// Get the number of levels in the Merkle tree
pub(crate) fn get_levels(env: &Env) -> Result<u32, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Levels)
        .ok_or(Error::NotInitialized)
}

/// Save the number of levels in the Merkle tree
pub(crate) fn set_levels(env: &Env, levels: u32) {
    env.storage().persistent().set(&DataKey::Levels, &levels);
}

// ========== Next index ==========

/// Get the next available leaf index
pub(crate) fn get_next_index(env: &Env) -> Result<u64, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::NextIndex)
        .ok_or(Error::NotInitialized)
}

/// Save the next available leaf index
pub(crate) fn set_next_index(env: &Env, index: u64) {
    env.storage().persistent().set(&DataKey::NextIndex, &index);
}

// ========== Root ==========

/// Get the current Merkle root
pub(crate) fn get_root(env: &Env) -> Result<U256, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Root)
        .ok_or(Error::NotInitialized)
}

/// Save the current Merkle root
pub(crate) fn set_root(env: &Env, root: &U256) {
    env.storage().persistent().set(&DataKey::Root, root);
}

// ========== Filled subtrees ==========

/// Get the filled subtree hash at the given level
pub(crate) fn get_filled_subtree(env: &Env, level: u32) -> Result<U256, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::FilledSubtrees(level))
        .ok_or(Error::NotInitialized)
}

/// Save the filled subtree hash at the given level
pub(crate) fn set_filled_subtree(env: &Env, level: u32, value: &U256) {
    env.storage()
        .persistent()
        .set(&DataKey::FilledSubtrees(level), value);
}

// ========== Zero hashes ==========

/// Get the precomputed zero hash at the given level
pub(crate) fn get_zero(env: &Env, level: u32) -> Result<U256, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Zeroes(level))
        .ok_or(Error::NotInitialized)
}

/// Save the precomputed zero hash at the given level
pub(crate) fn set_zero(env: &Env, level: u32, value: &U256) {
    env.storage()
        .persistent()
        .set(&DataKey::Zeroes(level), value);
}

// ========== Admin-insert-only flag ==========

/// Get whether admin permission is required to insert a leaf (defaults to true)
pub(crate) fn get_admin_insert_only(env: &Env) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::AdminInsertOnly)
        .unwrap_or(true)
}

/// Save whether admin permission is required to insert a leaf
pub(crate) fn set_admin_insert_only(env: &Env, admin_only: bool) {
    env.storage()
        .persistent()
        .set(&DataKey::AdminInsertOnly, &admin_only);
}
