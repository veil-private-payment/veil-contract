use soroban_sdk::{Address, Env, U256, Vec, contract, contractimpl};
use soroban_utils::{bn256_modulus, get_zeroes, poseidon2_compress};

use crate::error::Error;
use crate::event::LeafAddedEvent;
use crate::storage;
use crate::storage_types::DataKey;

#[contract]
pub struct ASPMembership;

#[contractimpl]
impl ASPMembership {
    /// Ensure a value is a valid element of the BN254 scalar field.
    fn ensure_field_element(env: &Env, value: &U256) -> Result<(), Error> {
        if value.clone() >= bn256_modulus(env) {
            return Err(Error::InvalidFieldElement);
        }
        Ok(())
    }

    /// Constructor: initialize the ASP Membership contract
    ///
    /// Creates a new Merkle tree with the specified number of levels and sets
    /// the admin address. The tree is initialized with zero hashes at each
    /// level.
    pub fn __constructor(env: Env, admin: Address, levels: u32) -> Result<(), Error> {
        if levels == 0 || levels > 32 {
            return Err(Error::WrongLevels);
        }

        // Initialize admin and tree parameters
        storage::set_admin(&env, &admin);
        storage::set_levels(&env, levels);
        storage::set_next_index(&env, 0u64);
        storage::set_admin_insert_only(&env, true);

        // Initialize an empty tree with zero hashes at each level
        let zeros: Vec<U256> = get_zeroes(&env);
        for lvl in 0..=levels {
            let zero_val = zeros.get(lvl).ok_or(Error::NotInitialized)?;
            storage::set_filled_subtree(&env, lvl, &zero_val);
            storage::set_zero(&env, lvl, &zero_val);
        }

        // Set initial root to the zero hash at the top level
        let root_val = zeros.get(levels).ok_or(Error::NotInitialized)?;
        storage::set_root(&env, &root_val);

        Ok(())
    }

    /// Insert a new leaf into the Merkle tree
    ///
    /// Adds a new member to the Merkle tree and updates the root. The leaf is
    /// inserted at the next available index and the tree is updated efficiently
    /// by only recomputing the hashes along the path to the root. If
    /// `admin_insert_only` is enabled (the default), only the admin can insert
    /// leaves; otherwise, anyone can call this function.
    pub fn insert_leaf(env: Env, leaf: U256) -> Result<(), Error> {
        Self::ensure_field_element(&env, &leaf)?;

        if storage::get_admin_insert_only(&env) {
            let admin = storage::get_admin(&env)?;
            admin.require_auth();
        }

        let levels = storage::get_levels(&env)?;
        let actual_index = storage::get_next_index(&env)?;
        let mut current_index = actual_index;

        // Check if tree is full (capacity is 2^levels leaves)
        if current_index >= 1u64.checked_shl(levels).ok_or(Error::MerkleTreeFull)? {
            return Err(Error::MerkleTreeFull);
        }
        let mut current_hash = leaf.clone();

        // Update tree by recomputing hashes along the path to root
        for lvl in 0..levels {
            let is_right = current_index & 1 == 1;
            if is_right {
                // Leaf is right child, get the stored left sibling
                let left = storage::get_filled_subtree(&env, lvl)?;
                current_hash = poseidon2_compress(&env, left, current_hash);
            } else {
                // Leaf is left child, store it and pair with zero hash
                storage::set_filled_subtree(&env, lvl, &current_hash);
                let zero_val = storage::get_zero(&env, lvl)?;
                current_hash = poseidon2_compress(&env, current_hash, zero_val);
            }
            current_index >>= 1;
        }

        storage::set_root(&env, &current_hash);

        LeafAddedEvent {
            leaf: leaf.clone(),
            index: actual_index,
            root: current_hash,
        }
        .publish(&env);

        // Advance the next-index cursor
        storage::set_next_index(&env, actual_index.checked_add(1).ok_or(Error::Overflow)?);

        Ok(())
    }

    //--------- Getters -----------

    pub fn get_root(env: Env) -> Result<U256, Error> {
        storage::get_root(&env)
    }

    /// Hash two U256 values using Poseidon2 compression
    ///
    /// Computes the Poseidon2 hash of two field elements in compression mode.
    /// This is the core hashing function used for Merkle tree operations.
    pub fn hash_pair(env: &Env, left: U256, right: U256) -> U256 {
        if let Err(err) = Self::ensure_field_element(env, &left) {
            env.panic_with_error(err);
        }
        if let Err(err) = Self::ensure_field_element(env, &right) {
            env.panic_with_error(err);
        }
        poseidon2_compress(env, left, right)
    }

    //--------- Administrative functions -----------

    /// Update the contract administrator
    ///
    /// Changes the admin address to a new address. Only the current admin
    /// can call this function.
    pub fn update_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        if !storage::has_admin(&env) {
            return Err(Error::NotInitialized);
        }
        soroban_utils::update_admin(&env, &DataKey::Admin, &new_admin);
        Ok(())
    }

    /// Set whether admin permission is required to insert a leaf
    ///
    /// When `admin_only` is true (default), only the admin can insert leaves.
    /// When false, anyone can insert leaves. Only the admin can change this
    /// setting.
    pub fn set_admin_insert_only(env: Env, admin_only: bool) -> Result<(), Error> {
        let admin = storage::get_admin(&env)?;
        admin.require_auth();
        storage::set_admin_insert_only(&env, admin_only);
        Ok(())
    }
}
