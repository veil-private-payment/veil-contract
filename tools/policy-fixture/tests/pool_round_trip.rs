//! End-to-end check that a real Groth16 proof settles through the pool.
//!
//! Everything here is real: the ASP membership contract, the Groth16 verifier
//! contract carrying the embedded verification key, and a proof produced from
//! the committed proving key over the compiled circuit. Nothing is mocked.
//!
//! The proof binds `extDataHash`, which the pool derives from the XDR encoding
//! of its own `ExtData`. That value only exists inside a Soroban environment,
//! so the test computes it first and proves against it.

use asp_membership::{ASPMembership, ASPMembershipClient};
use circom_groth16_verifier::CircomGroth16Verifier;
use contract_types::Groth16Proof;
use policy_fixture::{generate_with_ext_data_hash, generate_with_ext_data_hash_and_amount};
use pool::{
    PoolContract, PoolContractClient,
    contract::hash_ext_data,
    types::{ExtData, Proof},
};
use soroban_sdk::{
    Address, Bytes, Env, I256, U256, Vec, testutils::Address as _, token::StellarAssetClient,
};

const LEVELS: u32 = 10;

fn u256_from_be(env: &Env, bytes: &[u8; 32]) -> U256 {
    U256::from_be_bytes(env, &Bytes::from_array(env, bytes))
}

#[test]
fn a_real_proof_settles_a_shielded_transfer_through_the_pool() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let asset = StellarAssetClient::new(&env, &token);

    let asp_id = env.register(ASPMembership, (admin.clone(), LEVELS));
    let asp = ASPMembershipClient::new(&env, &asp_id);
    let verifier_id = env.register(CircomGroth16Verifier, ());

    let pool_id = env.register(
        PoolContract,
        (
            admin.clone(),
            token.clone(),
            verifier_id,
            asp_id,
            U256::from_u32(&env, 1_000_000),
            admin.clone(),
            0u32,
            LEVELS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    // A private send moves nothing publicly, so the external amount is zero.
    let ext_data = ExtData {
        recipient: admin.clone(),
        ext_amount: I256::from_i32(&env, 0),
        encrypted_output0: Bytes::new(&env),
        encrypted_output1: Bytes::new(&env),
    };
    let ext_hash = hash_ext_data(&env, &ext_data);

    let fixture = generate_with_ext_data_hash(ext_hash.to_array())
        .unwrap_or_else(|err| panic!("fixture generation should succeed: {err}"));

    // Rebuild the pool tree the proof was generated against. The fixture notes
    // sit on the even leaves that deposits fill.
    let sender = Address::generate(&env);
    asset.mint(&sender, &1_000);
    for commitment in fixture.input_commitments() {
        pool.deposit(&sender, &1, &u256_from_be(&env, commitment));
    }

    // Rebuild the allowlist the proof was generated against.
    for leaf in fixture.membership_leaves() {
        asp.insert_leaf(&u256_from_be(&env, leaf));
    }

    let public = fixture.public_inputs_be();
    assert_eq!(public.len(), 9);

    let root = u256_from_be(&env, &public[0]);
    let membership_root = u256_from_be(&env, &public[7]);
    assert_eq!(pool.get_root(), root);
    assert_eq!(pool.get_asp_membership_root(), membership_root);

    let nullifier0 = u256_from_be(&env, &public[3]);
    let nullifier1 = u256_from_be(&env, &public[4]);
    let commitment0 = u256_from_be(&env, &public[5]);
    let commitment1 = u256_from_be(&env, &public[6]);

    let mut input_nullifiers: Vec<U256> = Vec::new(&env);
    input_nullifiers.push_back(nullifier0.clone());
    input_nullifiers.push_back(nullifier1.clone());

    let proof_bytes = fixture.proof_bytes();
    let proof = Proof {
        proof: Groth16Proof::try_from(Bytes::from_slice(&env, &proof_bytes))
            .unwrap_or_else(|_| panic!("fixture proof should decode")),
        root,
        input_nullifiers,
        output_commitment0: commitment0.clone(),
        output_commitment1: commitment1.clone(),
        public_amount: u256_from_be(&env, &public[1]),
        ext_data_hash: ext_hash,
        asp_membership_root: membership_root,
    };

    pool.transact(&proof, &ext_data, &sender);

    assert!(pool.has_nullifier(&nullifier0));
    assert!(pool.has_nullifier(&nullifier1));
    assert!(pool.has_commitment(&commitment0));
    assert!(pool.has_commitment(&commitment1));
}

/// The same real proof must be rejected when the spender is not enrolled.
///
/// The pool binds the proof to the live allowlist root, so an ASP contract
/// that never enrolled the spender reports a different root and the spend is
/// refused before the pairing check even runs.
#[test]
fn a_real_proof_is_rejected_when_the_spender_is_not_enrolled() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let asset = StellarAssetClient::new(&env, &token);

    let asp_id = env.register(ASPMembership, (admin.clone(), LEVELS));
    let verifier_id = env.register(CircomGroth16Verifier, ());

    let pool_id = env.register(
        PoolContract,
        (
            admin.clone(),
            token.clone(),
            verifier_id,
            asp_id,
            U256::from_u32(&env, 1_000_000),
            admin.clone(),
            0u32,
            LEVELS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    let ext_data = ExtData {
        recipient: admin.clone(),
        ext_amount: I256::from_i32(&env, 0),
        encrypted_output0: Bytes::new(&env),
        encrypted_output1: Bytes::new(&env),
    };
    let ext_hash = hash_ext_data(&env, &ext_data);

    let fixture = generate_with_ext_data_hash(ext_hash.to_array())
        .unwrap_or_else(|err| panic!("fixture generation should succeed: {err}"));

    let sender = Address::generate(&env);
    asset.mint(&sender, &1_000);
    for commitment in fixture.input_commitments() {
        pool.deposit(&sender, &1, &u256_from_be(&env, commitment));
    }

    // Deliberately skip enrollment: the allowlist stays empty.

    let public = fixture.public_inputs_be();
    let membership_root = u256_from_be(&env, &public[7]);
    assert_ne!(pool.get_asp_membership_root(), membership_root);

    let mut input_nullifiers: Vec<U256> = Vec::new(&env);
    input_nullifiers.push_back(u256_from_be(&env, &public[3]));
    input_nullifiers.push_back(u256_from_be(&env, &public[4]));

    let proof_bytes = fixture.proof_bytes();
    let proof = Proof {
        proof: Groth16Proof::try_from(Bytes::from_slice(&env, &proof_bytes))
            .unwrap_or_else(|_| panic!("fixture proof should decode")),
        root: u256_from_be(&env, &public[0]),
        input_nullifiers,
        output_commitment0: u256_from_be(&env, &public[5]),
        output_commitment1: u256_from_be(&env, &public[6]),
        public_amount: u256_from_be(&env, &public[1]),
        ext_data_hash: ext_hash,
        asp_membership_root: membership_root,
    };

    assert!(pool.try_transact(&proof, &ext_data, &sender).is_err());
    assert!(!pool.has_nullifier(&u256_from_be(&env, &public[3])));
}

/// A withdrawal carrying a real proof pays the recipient and the fee recipient.
///
/// The circuit enforces `sumIns + publicAmount == sumOuts`, so a withdrawal
/// shrinks the shielded output by the amount leaving the pool. The pool
/// independently derives the same public amount from `ExtData`, and the proof
/// only verifies if the two agree.
#[test]
fn a_real_proof_withdraws_and_splits_the_protocol_fee() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let asset = StellarAssetClient::new(&env, &token);

    let asp_id = env.register(ASPMembership, (admin.clone(), LEVELS));
    let asp = ASPMembershipClient::new(&env, &asp_id);
    let verifier_id = env.register(CircomGroth16Verifier, ());
    let fee_recipient = Address::generate(&env);
    let recipient = Address::generate(&env);

    // 1000 bps of 10 is 1, leaving 9 for the recipient.
    let pool_id = env.register(
        PoolContract,
        (
            admin.clone(),
            token.clone(),
            verifier_id,
            asp_id,
            U256::from_u32(&env, 1_000_000),
            fee_recipient.clone(),
            1_000u32,
            LEVELS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    let ext_data = ExtData {
        recipient: recipient.clone(),
        ext_amount: I256::from_i32(&env, -10),
        encrypted_output0: Bytes::new(&env),
        encrypted_output1: Bytes::new(&env),
    };
    let ext_hash = hash_ext_data(&env, &ext_data);

    let fixture = generate_with_ext_data_hash_and_amount(ext_hash.to_array(), -10)
        .unwrap_or_else(|err| panic!("fixture generation should succeed: {err}"));

    let sender = Address::generate(&env);
    asset.mint(&sender, &1_000);
    for commitment in fixture.input_commitments() {
        pool.deposit(&sender, &50, &u256_from_be(&env, commitment));
    }
    for leaf in fixture.membership_leaves() {
        asp.insert_leaf(&u256_from_be(&env, leaf));
    }

    let public = fixture.public_inputs_be();
    let public_amount = u256_from_be(&env, &public[1]);
    assert_eq!(pool.get_public_amount(&ext_data.ext_amount), public_amount);

    let mut input_nullifiers: Vec<U256> = Vec::new(&env);
    input_nullifiers.push_back(u256_from_be(&env, &public[3]));
    input_nullifiers.push_back(u256_from_be(&env, &public[4]));

    let proof_bytes = fixture.proof_bytes();
    let proof = Proof {
        proof: Groth16Proof::try_from(Bytes::from_slice(&env, &proof_bytes))
            .unwrap_or_else(|_| panic!("fixture proof should decode")),
        root: u256_from_be(&env, &public[0]),
        input_nullifiers,
        output_commitment0: u256_from_be(&env, &public[5]),
        output_commitment1: u256_from_be(&env, &public[6]),
        public_amount,
        ext_data_hash: ext_hash,
        asp_membership_root: u256_from_be(&env, &public[7]),
    };

    let token_client = soroban_sdk::token::TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&pool_id), 100);

    pool.transact(&proof, &ext_data, &sender);

    assert_eq!(token_client.balance(&recipient), 9);
    assert_eq!(token_client.balance(&fee_recipient), 1);
    assert_eq!(token_client.balance(&pool_id), 90);
}
