use crate::{
    PoolContract, PoolContractClient,
    merkle_with_history::{MerkleDataKey, MerkleTreeWithHistory},
    types::{Account, ExtData, Proof},
    verifier_boundary::VerifierPublicInputs,
};
use asp_membership::{ASPMembership, ASPMembershipClient};
use contract_types::Groth16Proof;
use mock_verifier::MockGroth16Verifier;
use soroban_sdk::{
    Address, Bytes, BytesN, Env, I256, IntoVal, Map, Symbol, TryFromVal, U256, Val, Vec,
    crypto::bn254::{Bn254Fr, Bn254G1Affine as G1Affine, Bn254G2Affine as G2Affine},
    symbol_short,
    testutils::{Address as _, Events as _},
    token::{StellarAssetClient, TokenClient},
    vec,
    xdr::{self, ToXdr},
};
use soroban_utils::constants::bn256_modulus;

/// Number of levels for the ASP Membership Merkle tree in tests
const ASP_MEMBERSHIP_LEVELS: u32 = 8;

struct TestSetup {
    admin: Address,
    token: Address,
    verifier: Address,
    asp_membership_address: Address,
    asp_membership_client: ASPMembershipClient<'static>,
}

/// Deploy the contracts the pool talks to.
///
/// The ASP membership contract is deployed for real because the pool reads its
/// root through a cross-contract call. The verifier is still a generated
/// address: nothing calls it until shielded transactions land.
fn setup_test_contracts(env: &Env) -> TestSetup {
    let admin = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let asp_membership_address =
        env.register(ASPMembership, (admin.clone(), ASP_MEMBERSHIP_LEVELS));
    let asp_membership_client = ASPMembershipClient::new(env, &asp_membership_address);

    TestSetup {
        admin,
        token,
        verifier: Address::generate(env),
        asp_membership_address,
        asp_membership_client,
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

fn assert_contract_event(env: &Env, pool_id: &Address, topics: Vec<Val>, data: Val) {
    let expected_topics = topics.into();
    let expected_data = xdr::ScVal::try_from_val(env, &data)
        .unwrap_or_else(|_| panic!("expected event data should convert to XDR"));

    let pool_events = env.events().all().filter_by_contract(pool_id);
    assert!(
        pool_events.events().iter().any(|event| {
            let xdr::ContractEventBody::V0(body) = &event.body;
            body.topics == expected_topics && body.data == expected_data
        }),
        "expected contract event was not emitted; events: {pool_events:?}",
    );
}

fn assert_deposit_event(
    env: &Env,
    pool_id: &Address,
    asset: &Address,
    commitment: U256,
    index: u32,
    amount_bucket: i128,
) {
    let pool_events = env.events().all().filter_by_contract(pool_id);
    assert_eq!(
        pool_events,
        vec![
            env,
            (
                pool_id.clone(),
                (symbol_short!("Deposit"), commitment, pool_id.clone()).into_val(env),
                Map::<Symbol, Val>::from_array(
                    env,
                    [
                        (
                            Symbol::new(env, "amount_bucket"),
                            amount_bucket.into_val(env),
                        ),
                        (symbol_short!("asset"), asset.clone().into_val(env)),
                        (symbol_short!("index"), index.into_val(env)),
                    ],
                )
                .into_val(env),
            )
        ]
    );
}

fn assert_deposit_event_present(
    env: &Env,
    pool_id: &Address,
    asset: &Address,
    commitment: U256,
    index: u32,
    amount_bucket: i128,
) {
    let topics: Vec<Val> = (symbol_short!("Deposit"), commitment, pool_id.clone()).into_val(env);
    let data: Val = Map::<Symbol, Val>::from_array(
        env,
        [
            (
                Symbol::new(env, "amount_bucket"),
                amount_bucket.into_val(env),
            ),
            (symbol_short!("asset"), asset.clone().into_val(env)),
            (symbol_short!("index"), index.into_val(env)),
        ],
    )
    .into_val(env);
    assert_contract_event(env, pool_id, topics, data);
}

fn assert_public_key_event(
    env: &Env,
    pool_id: &Address,
    owner: &Address,
    encryption_key: &Bytes,
    note_key: &Bytes,
) {
    let topics: Vec<Val> = (Symbol::new(env, "public_key_event"), owner.clone()).into_val(env);
    let data: Val = Map::<Symbol, Val>::from_array(
        env,
        [
            (
                Symbol::new(env, "encryption_key"),
                encryption_key.clone().into_val(env),
            ),
            (symbol_short!("note_key"), note_key.clone().into_val(env)),
        ],
    )
    .into_val(env);
    assert_contract_event(env, pool_id, topics, data);
}

fn mk_bytesn32(env: &Env, fill: u8) -> BytesN<32> {
    BytesN::from_array(env, &[fill; 32])
}

fn mk_ext_data(env: &Env, recipient: Address, ext_amount: i32) -> ExtData {
    ExtData {
        recipient,
        ext_amount: I256::from_i32(env, ext_amount),
        encrypted_output0: Bytes::new(env),
        encrypted_output1: Bytes::new(env),
    }
}

fn compute_ext_hash(env: &Env, ext: &ExtData) -> BytesN<32> {
    let payload = ext.clone().to_xdr(env);
    let digest: BytesN<32> = env.crypto().keccak256(&payload).into();
    let digest_u256 = U256::from_be_bytes(env, &Bytes::from(digest));
    let reduced = digest_u256.rem_euclid(&bn256_modulus(env));
    let mut buf = [0u8; 32];
    reduced.to_be_bytes().copy_into_slice(&mut buf);
    BytesN::from_array(env, &buf)
}

fn fr_from_u256(env: &Env, value: &U256) -> Bn254Fr {
    let mut buf = [0u8; 32];
    value.to_be_bytes().copy_into_slice(&mut buf);
    Bn254Fr::from_bytes(BytesN::from_array(env, &buf))
}

/// The deterministic proof shape accepted by the mock verifier.
fn mk_mock_groth16_proof(env: &Env) -> Groth16Proof {
    let g1_bytes = {
        let mut bytes = [0u8; 64];
        bytes[31] = 1;
        bytes[63] = 2;
        bytes
    };
    let g2_bytes = {
        let mut bytes = [0u8; 128];
        bytes[31] = 1;
        bytes[63] = 1;
        bytes[95] = 1;
        bytes[127] = 1;
        bytes
    };

    Groth16Proof {
        a: G1Affine::from_array(env, &g1_bytes),
        b: G2Affine::from_array(env, &g2_bytes),
        c: G1Affine::from_array(env, &g1_bytes),
    }
}

/// Deploy the pool against the demo-only mock verifier.
///
/// The mock verifier checks the public input count and one fixed proof shape.
/// It exists so the transaction pipeline can be tested without a real proving
/// run; it performs no Groth16 verification.
fn setup_test_contracts_with_mock_verifier(env: &Env) -> TestSetup {
    let mut setup = setup_test_contracts(env);
    setup.verifier = env.register(MockGroth16Verifier, ());
    setup
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

#[test]
fn register_emits_public_key_event_schema() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let owner = Address::generate(&env);
    let encryption_key = Bytes::from_array(&env, &[7u8; 32]);
    let note_key = Bytes::from_array(&env, &[9u8; 32]);
    let account = Account {
        owner: owner.clone(),
        encryption_key: encryption_key.clone(),
        note_key: note_key.clone(),
    };

    pool.register(&account);

    assert_public_key_event(&env, &pool_id, &owner, &encryption_key, &note_key);
}

#[test]
fn deposit_transfers_tokens_and_returns_commitment_index() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 1_000);
    let pool_id = register_pool(&env, &setup, max, 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let token = TokenClient::new(&env, &setup.token);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);
    let commitment = U256::from_u32(&env, 0xCAFE);

    asset.mint(&sender, &500);
    let root_before = pool.get_root();

    let commitment_index = pool.deposit(&sender, &125, &commitment);

    assert_eq!(commitment_index, 0);
    assert!(pool.has_commitment(&commitment));
    assert_eq!(token.balance(&sender), 375);
    assert_eq!(token.balance(&pool_id), 125);
    assert_ne!(pool.get_root(), root_before);

    let next_index: u64 = env.as_contract(&pool_id, || {
        env.storage()
            .persistent()
            .get(&MerkleDataKey::NextIndex)
            .unwrap_or_else(|| panic!("expected next index to be stored"))
    });
    assert_eq!(next_index, 2);
}

#[test]
fn deposit_emits_indexable_event_without_sender_metadata() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 1_000);
    let pool_id = register_pool(&env, &setup, max, 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);
    let commitment = U256::from_u32(&env, 0xD06);

    asset.mint(&sender, &500);

    assert_eq!(pool.deposit(&sender, &125, &commitment), 0);

    assert_deposit_event(&env, &pool_id, &setup.token, commitment, 0, 125_i128);
}

#[test]
fn deposit_keeps_stable_commitment_indices_across_multiple_deposits() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let token = TokenClient::new(&env, &setup.token);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);
    let commitment0 = U256::from_u32(&env, 0xD10);
    let commitment1 = U256::from_u32(&env, 0xD11);

    asset.mint(&sender, &500);
    let root_before = pool.get_root();

    let index0 = pool.deposit(&sender, &125, &commitment0);
    assert_eq!(index0, 0);
    assert_deposit_event_present(
        &env,
        &pool_id,
        &setup.token,
        commitment0.clone(),
        index0,
        125_i128,
    );
    let root_after_first = pool.get_root();

    let index1 = pool.deposit(&sender, &75, &commitment1);
    assert_eq!(index1, 2);
    assert_deposit_event_present(
        &env,
        &pool_id,
        &setup.token,
        commitment1.clone(),
        index1,
        75_i128,
    );

    assert!(pool.has_commitment(&commitment0));
    assert!(pool.has_commitment(&commitment1));
    assert_eq!(token.balance(&sender), 300);
    assert_eq!(token.balance(&pool_id), 200);
    assert_ne!(root_after_first, root_before);
    assert_ne!(pool.get_root(), root_after_first);
}

#[test]
fn deposit_rejects_duplicate_commitment_without_transferring_again() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 1_000);
    let pool_id = register_pool(&env, &setup, max, 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let token = TokenClient::new(&env, &setup.token);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);
    let commitment = U256::from_u32(&env, 0xBEEF);

    asset.mint(&sender, &500);

    assert_eq!(pool.deposit(&sender, &125, &commitment), 0);
    assert!(pool.try_deposit(&sender, &125, &commitment).is_err());

    assert_eq!(token.balance(&sender), 375);
    assert_eq!(token.balance(&pool_id), 125);
}

#[test]
fn deposit_rejects_invalid_amounts_without_transferring() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 100);
    let pool_id = register_pool(&env, &setup, max, 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let token = TokenClient::new(&env, &setup.token);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);

    asset.mint(&sender, &500);

    assert!(
        pool.try_deposit(&sender, &0, &U256::from_u32(&env, 0x01))
            .is_err()
    );
    assert!(
        pool.try_deposit(&sender, &101, &U256::from_u32(&env, 0x02))
            .is_err()
    );

    assert_eq!(token.balance(&sender), 500);
    assert_eq!(token.balance(&pool_id), 0);
}

#[test]
fn deposit_rejects_commitment_outside_bn254_field_without_transferring() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let max = U256::from_u32(&env, 1_000);
    let pool_id = register_pool(&env, &setup, max, 8);
    let pool = PoolContractClient::new(&env, &pool_id);
    let token = TokenClient::new(&env, &setup.token);
    let asset = StellarAssetClient::new(&env, &setup.token);
    let sender = Address::generate(&env);
    let invalid_commitment = bn256_modulus(&env);

    asset.mint(&sender, &500);

    assert!(
        pool.try_deposit(&sender, &125, &invalid_commitment)
            .is_err()
    );

    assert!(!pool.has_commitment(&invalid_commitment));
    assert_eq!(token.balance(&sender), 500);
    assert_eq!(token.balance(&pool_id), 0);
}

#[test]
fn has_nullifier_is_false_before_any_spend() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    assert!(!pool.has_nullifier(&U256::from_u32(&env, 0x01)));
}

#[test]
fn get_asp_membership_root_reads_through_to_the_asp_contract() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    assert_eq!(
        pool.get_asp_membership_root(),
        setup.asp_membership_client.get_root()
    );
}

#[test]
fn get_asp_membership_root_tracks_enrollment() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    let empty_root = pool.get_asp_membership_root();

    setup
        .asp_membership_client
        .insert_leaf(&U256::from_u32(&env, 0xA11CE));

    let enrolled_root = pool.get_asp_membership_root();
    assert_ne!(enrolled_root, empty_root);
    assert_eq!(enrolled_root, setup.asp_membership_client.get_root());
}

#[test]
fn update_asp_membership_repoints_the_root_lookup() {
    let env = test_env();
    env.mock_all_auths();

    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    setup
        .asp_membership_client
        .insert_leaf(&U256::from_u32(&env, 0xB0B));
    let enrolled_root = pool.get_asp_membership_root();

    // A freshly deployed ASP contract is empty, so the pool must report the
    // empty root once it is repointed.
    let fresh_asp = env.register(ASPMembership, (setup.admin.clone(), ASP_MEMBERSHIP_LEVELS));
    let fresh_client = ASPMembershipClient::new(&env, &fresh_asp);
    pool.update_asp_membership(&fresh_asp);

    assert_ne!(pool.get_asp_membership_root(), enrolled_root);
    assert_eq!(pool.get_asp_membership_root(), fresh_client.get_root());
}

#[test]
fn verifier_public_inputs_follow_policy_transaction_order() {
    let env = test_env();
    let ext_data_hash = mk_bytesn32(&env, 0x77);
    let root = U256::from_u32(&env, 1);
    let public_amount = U256::from_u32(&env, 2);
    let nullifier0 = U256::from_u32(&env, 3);
    let nullifier1 = U256::from_u32(&env, 4);
    let output_commitment0 = U256::from_u32(&env, 5);
    let output_commitment1 = U256::from_u32(&env, 6);
    let asp_membership_root = U256::from_u32(&env, 7);

    let proof = Proof {
        proof: mk_mock_groth16_proof(&env),
        root: root.clone(),
        input_nullifiers: vec![&env, nullifier0.clone(), nullifier1.clone()],
        output_commitment0: output_commitment0.clone(),
        output_commitment1: output_commitment1.clone(),
        public_amount: public_amount.clone(),
        ext_data_hash: ext_data_hash.clone(),
        asp_membership_root: asp_membership_root.clone(),
    };

    let inputs = VerifierPublicInputs::from_proof(&env, &proof).values;
    let expected = vec![
        &env,
        fr_from_u256(&env, &root),
        fr_from_u256(&env, &public_amount),
        Bn254Fr::from_bytes(ext_data_hash),
        fr_from_u256(&env, &nullifier0),
        fr_from_u256(&env, &nullifier1),
        fr_from_u256(&env, &output_commitment0),
        fr_from_u256(&env, &output_commitment1),
        fr_from_u256(&env, &asp_membership_root),
        fr_from_u256(&env, &asp_membership_root),
    ];

    // Nine values, matching the circuit's public input count.
    assert_eq!(inputs.len(), 9);
    assert_eq!(inputs, expected);
}

struct TransactCase {
    pool_id: Address,
    sender: Address,
    ext: ExtData,
    proof: Proof,
}

/// Build a pool on the mock verifier plus a matching well-formed transaction.
fn mk_transact_case(env: &Env, setup: &TestSetup) -> TransactCase {
    let pool_id = register_pool(env, setup, U256::from_u32(env, 1_000), 8);
    let pool = PoolContractClient::new(env, &pool_id);

    let ext = mk_ext_data(env, Address::generate(env), 0);
    let proof = Proof {
        proof: mk_mock_groth16_proof(env),
        root: pool.get_root(),
        input_nullifiers: vec![env, U256::from_u32(env, 0x101), U256::from_u32(env, 0x102)],
        output_commitment0: U256::from_u32(env, 0x201),
        output_commitment1: U256::from_u32(env, 0x202),
        public_amount: U256::from_u32(env, 0),
        ext_data_hash: compute_ext_hash(env, &ext),
        asp_membership_root: setup.asp_membership_client.get_root(),
    };

    TransactCase {
        pool_id,
        sender: Address::generate(env),
        ext,
        proof,
    }
}

#[test]
fn transact_settles_against_the_mock_verifier() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    let nullifier0 = case.proof.input_nullifiers.get_unchecked(0);
    let nullifier1 = case.proof.input_nullifiers.get_unchecked(1);
    let commitment0 = case.proof.output_commitment0.clone();
    let commitment1 = case.proof.output_commitment1.clone();

    pool.transact(&case.proof, &case.ext, &case.sender);

    assert!(pool.has_nullifier(&nullifier0));
    assert!(pool.has_nullifier(&nullifier1));
    assert!(pool.has_commitment(&commitment0));
    assert!(pool.has_commitment(&commitment1));
}

#[test]
fn transact_rejects_a_spend_whose_asp_root_is_not_the_live_allowlist() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    // A spender who is not enrolled cannot prove against the live root, so the
    // only proof they can build carries a root of their own. The pool compares
    // the proof's root against the live allowlist and rejects it.
    let mut proof = case.proof;
    proof.asp_membership_root = U256::from_u32(&env, 0xDEAD);

    let nullifier0 = proof.input_nullifiers.get_unchecked(0);
    let commitment0 = proof.output_commitment0.clone();

    assert!(pool.try_transact(&proof, &case.ext, &case.sender).is_err());

    assert!(!pool.has_nullifier(&nullifier0));
    assert!(!pool.has_commitment(&commitment0));
}

#[test]
fn transact_rejects_a_stale_asp_root_after_a_new_enrollment() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    // The allowlist moves on after the proof was built.
    setup
        .asp_membership_client
        .insert_leaf(&U256::from_u32(&env, 0xA11CE));

    assert!(
        pool.try_transact(&case.proof, &case.ext, &case.sender)
            .is_err()
    );
}

#[test]
fn transact_rejects_unknown_root() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    let mut proof = case.proof;
    proof.root = U256::from_u32(&env, 0xBAD1);

    assert!(pool.try_transact(&proof, &case.ext, &case.sender).is_err());
}

#[test]
fn transact_rejects_bad_ext_hash() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    let mut proof = case.proof;
    proof.ext_data_hash = mk_bytesn32(&env, 0x01);

    assert!(pool.try_transact(&proof, &case.ext, &case.sender).is_err());
}

#[test]
fn transact_rejects_bad_public_amount() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    let mut proof = case.proof;
    proof.public_amount = U256::from_u32(&env, 7);

    assert!(pool.try_transact(&proof, &case.ext, &case.sender).is_err());
}

#[test]
fn transact_rejects_a_replayed_nullifier() {
    let env = test_env();
    env.mock_all_auths();
    let setup = setup_test_contracts_with_mock_verifier(&env);
    let case = mk_transact_case(&env, &setup);
    let pool = PoolContractClient::new(&env, &case.pool_id);

    pool.transact(&case.proof, &case.ext, &case.sender);

    // Same nullifiers, fresh output commitments.
    let mut replay = Proof {
        proof: mk_mock_groth16_proof(&env),
        root: pool.get_root(),
        input_nullifiers: case.proof.input_nullifiers.clone(),
        output_commitment0: U256::from_u32(&env, 0x301),
        output_commitment1: U256::from_u32(&env, 0x302),
        public_amount: U256::from_u32(&env, 0),
        ext_data_hash: case.proof.ext_data_hash.clone(),
        asp_membership_root: setup.asp_membership_client.get_root(),
    };
    replay.root = pool.get_root();

    assert!(pool.try_transact(&replay, &case.ext, &case.sender).is_err());
}

#[test]
fn get_ext_data_hash_matches_the_binding_hash() {
    let env = test_env();
    let setup = setup_test_contracts(&env);
    let pool_id = register_pool(&env, &setup, U256::from_u32(&env, 1_000), 8);
    let pool = PoolContractClient::new(&env, &pool_id);

    let ext = mk_ext_data(&env, Address::generate(&env), -5);

    assert_eq!(pool.get_ext_data_hash(&ext), compute_ext_hash(&env, &ext));
}
