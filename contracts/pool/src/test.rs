use crate::{
    PoolContract, PoolContractClient,
    merkle_with_history::{MerkleDataKey, MerkleTreeWithHistory},
};
use soroban_sdk::{Address, Env, U256, testutils::Address as _};

struct TestSetup {
    admin: Address,
    token: Address,
    verifier: Address,
    asp_membership_address: Address,
}

/// Deploy the token contract and generate the addresses the pool stores.
///
/// The constructor only records the verifier and ASP membership addresses, so
/// these tests do not need those contracts deployed. Tests that make
/// cross-contract calls register the real contracts instead.
fn setup_test_contracts(env: &Env) -> TestSetup {
    let admin = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    TestSetup {
        admin,
        token,
        verifier: Address::generate(env),
        asp_membership_address: Address::generate(env),
    }
}

/// Register the pool with the MVP default fee configuration used by most tests.
fn register_pool(
    env: &Env,
    setup: &TestSetup,
    maximum_deposit_amount: U256,
    levels: u32,
) -> Address {
    register_pool_with_fee(
        env,
        setup,
        maximum_deposit_amount,
        setup.admin.clone(),
        0u32,
        levels,
    )
}

fn register_pool_with_fee(
    env: &Env,
    setup: &TestSetup,
    maximum_deposit_amount: U256,
    fee_recipient: Address,
    fee_bps: u32,
    levels: u32,
) -> Address {
    env.register(
        PoolContract,
        (
            setup.admin.clone(),
            setup.token.clone(),
            setup.verifier.clone(),
            setup.asp_membership_address.clone(),
            maximum_deposit_amount,
            fee_recipient,
            fee_bps,
            levels,
        ),
    )
}

/// Create a test environment that disables snapshot writing under Miri.
/// Miri's isolation mode blocks filesystem operations, which the Soroban SDK
/// uses for test snapshots.
fn test_env() -> Env {
    #[cfg(miri)]
    {
        use soroban_sdk::testutils::EnvTestConfig;
        Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        })
    }
    #[cfg(not(miri))]
    {
        Env::default()
    }
}

#[test]
fn pool_constructor_sets_state_and_config_view() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let levels = 8u32;
    let pool_id = register_pool(&env, &setup, max.clone(), levels);
    let pool = PoolContractClient::new(&env, &pool_id);

    let stored_admin: Address = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .get(&crate::storage_types::DataKey::Admin)
            .unwrap_or_else(|| panic!("expected admin to be stored"))
    });
    let stored_paused: bool = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .get(&crate::storage_types::DataKey::Paused)
            .unwrap_or_else(|| panic!("expected paused flag to be stored"))
    });
    let has_nullifiers = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .has(&crate::storage_types::DataKey::Nullifiers)
    });
    let has_commitments = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .has(&crate::storage_types::DataKey::Commitments)
    });
    let has_merkle_root = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .has(&MerkleDataKey::CurrentRootIndex)
    });

    assert_eq!(stored_admin, setup.admin);
    assert!(!stored_paused);
    assert!(has_nullifiers);
    assert!(has_commitments);
    assert!(has_merkle_root);

    let config = pool.get_config();
    assert_eq!(config.admin, setup.admin);
    assert_eq!(config.token, setup.token);
    assert_eq!(config.verifier, setup.verifier);
    assert_eq!(config.asp_membership, setup.asp_membership_address);
    assert_eq!(config.maximum_deposit_amount, max);
    assert_eq!(config.fee_recipient, setup.admin);
    assert_eq!(config.fee_bps, 0);
    assert!(!config.paused);

    let _root = pool.get_root();
}

#[test]
fn pool_constructor_accepts_custom_fee_config() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let fee_recipient = Address::generate(&env);
    let fee_bps = 25u32;
    let pool_id =
        register_pool_with_fee(&env, &setup, max.clone(), fee_recipient.clone(), fee_bps, 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    let config = pool.get_config();
    assert_eq!(config.maximum_deposit_amount, max);
    assert_eq!(config.fee_recipient, fee_recipient);
    assert_eq!(config.fee_bps, fee_bps);
    assert!(!config.paused);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn pool_constructor_rejects_invalid_fee_bps() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);

    register_pool_with_fee(&env, &setup, max, setup.admin.clone(), 10_001u32, 8);
}

#[test]
fn update_verifier_swaps_verifier_address_for_admin() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    assert_eq!(pool.get_config().verifier, setup.verifier);

    let new_verifier = Address::generate(&env);
    pool.update_verifier(&new_verifier);

    assert_eq!(pool.get_config().verifier, new_verifier);
}

#[test]
fn update_asp_membership_swaps_address_for_admin() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    assert_eq!(
        pool.get_config().asp_membership,
        setup.asp_membership_address
    );

    let new_asp = Address::generate(&env);
    pool.update_asp_membership(&new_asp);

    assert_eq!(pool.get_config().asp_membership, new_asp);
}

#[test]
fn update_admin_transfers_control() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    let new_admin = Address::generate(&env);
    pool.update_admin(&new_admin);

    assert_eq!(pool.get_config().admin, new_admin);
}

#[test]
fn merkle_init_only_once() {
    let env = test_env();
    // As MerkleTreeWithHistory is now a module
    // We need to register the contract first to access the env.storage of a smart
    // contract
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let levels = 8u32;
    // First init should succeed
    let pool_id = register_pool(&env, &setup, max, levels);

    env.as_contract(&pool_id, || {
        // Second init should return AlreadyInitialized error
        let result = MerkleTreeWithHistory::init(&env, levels);
        assert!(result.is_err());
    });
}

#[test]
fn merkle_insert_updates_root_and_index() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let levels = 8u32;
    let pool_id = register_pool(&env, &setup, max, levels);

    env.as_contract(&pool_id, || {
        let leaf1 = U256::from_u32(&env, 0x01);
        let leaf2 = U256::from_u32(&env, 0x02);

        let (idx_0, idx_1) = MerkleTreeWithHistory::insert_two_leaves(&env, leaf1, leaf2)
            .unwrap_or_else(|err| panic!("expected leaf insertion to succeed: {err:?}"));
        assert_eq!(idx_0, 0);
        assert_eq!(idx_1, 1);

        // last root must be known
        let root = MerkleTreeWithHistory::get_last_root(&env)
            .unwrap_or_else(|err| panic!("expected last root to exist: {err:?}"));
        assert!(
            MerkleTreeWithHistory::is_known_root(&env, &root)
                .unwrap_or_else(|err| panic!("expected root lookup to succeed: {err:?}"))
        );

        // nextIndex should now be 2 (stored in persistent storage)
        let next: u64 = env
            .storage()
            .persistent()
            .get(&MerkleDataKey::NextIndex)
            .unwrap_or_else(|| panic!("expected next index to be stored"));
        assert_eq!(next, 2);
    });
}

#[test]
fn merkle_insert_fails_when_full() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let levels = 1u32;
    let pool_id = register_pool(&env, &setup, max, levels);

    env.as_contract(&pool_id, || {
        let leaf1 = U256::from_u32(&env, 0x0A);
        let leaf2 = U256::from_u32(&env, 0x0B);

        // First insert should succeed
        let result1 = MerkleTreeWithHistory::insert_two_leaves(&env, leaf1.clone(), leaf2.clone());
        assert!(result1.is_ok());

        // Second insert should fail with MerkleTreeFull error
        let result2 = MerkleTreeWithHistory::insert_two_leaves(&env, leaf1, leaf2);
        assert!(result2.is_err());
    });
}

#[test]
fn merkle_init_rejects_zero_levels() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let levels = 8u32;
    let pool_id = register_pool(&env, &setup, max, levels);
    let levels = 0u32;

    env.as_contract(&pool_id, || {
        let result = MerkleTreeWithHistory::init(&env, levels);
        assert!(result.is_err());
    });
}
