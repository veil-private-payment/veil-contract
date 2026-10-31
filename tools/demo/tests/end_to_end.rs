//! A shield then a private payment, proved with fresh keys and settled
//! through the real pool, allowlist and Groth16 verifier contracts.
//!
//! This is what `veil-demo shield` and `veil-demo pay` do, minus the network:
//! paths come from the same `veil_indexer::tree` code the API serves them
//! with, the external data hash from `veil_demo::ext_data`, and the notes from
//! `veil_demo::note_crypto`. If any of those disagreed with the circuit or the
//! contracts, the pool would refuse the transaction here.
//!
//! Needs the compiled circuit (`make compile-policy-circuit`), so CI runs it in
//! the circuit job.

use asp_membership::{ASPMembership, ASPMembershipClient};
use circom_groth16_verifier::CircomGroth16Verifier;
use contract_types::Groth16Proof;
use policy_fixture::{
    CreatedNote, Fixture, Membership, NoteScalar, SpentNote, TREE_LEVELS, Transaction,
    membership_leaf, note_commitment, note_nullifier, note_public_key, prove_transaction,
    scalar_from_be_bytes, scalar_to_be_bytes,
};
use pool::types::{ExtData as PoolExtData, Proof};
use pool::{PoolContract, PoolContractClient, contract::hash_ext_data};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Bytes, Env, I256, U256, Vec as SorobanVec, xdr};
use veil_demo::ext_data::ExtData;
use veil_demo::note_crypto::{self, Opened};
use veil_indexer::event::U256Be;
use veil_indexer::tree::{Field, path};
use x25519_dalek::{PublicKey, StaticSecret};

const LEVELS: u32 = 16;

fn seeded_scalar(seed: u8) -> NoteScalar {
    scalar_from_be_bytes(&[seed; 32])
}

fn field(value: NoteScalar) -> Field {
    Field::from_u256(&U256Be(scalar_to_be_bytes(value))).expect("field element")
}

fn scalar(value: Field) -> NoteScalar {
    scalar_from_be_bytes(value.to_u256().as_bytes())
}

fn u256(env: &Env, bytes: &[u8; 32]) -> U256 {
    U256::from_be_bytes(env, &Bytes::from_array(env, bytes))
}

fn strkey(address: &Address) -> String {
    xdr::ScAddress::from(address).to_string()
}

fn amount_bytes(amount: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&amount.to_be_bytes());
    out
}

fn pool_ext_data(env: &Env, ext: &ExtData) -> PoolExtData {
    PoolExtData {
        recipient: Address::from_str(env, &ext.recipient),
        ext_amount: I256::from_i128(env, ext.ext_amount),
        encrypted_output0: Bytes::from_slice(env, &ext.encrypted_output0),
        encrypted_output1: Bytes::from_slice(env, &ext.encrypted_output1),
    }
}

fn soroban_proof(env: &Env, fixture: &Fixture) -> Proof {
    let public = fixture.public_inputs_be();
    let mut nullifiers = SorobanVec::new(env);
    nullifiers.push_back(u256(env, &public[3]));
    nullifiers.push_back(u256(env, &public[4]));
    Proof {
        proof: Groth16Proof::try_from(Bytes::from_slice(env, &fixture.proof_bytes()))
            .unwrap_or_else(|_| panic!("proof decodes")),
        root: u256(env, &public[0]),
        input_nullifiers: nullifiers,
        output_commitment0: u256(env, &public[5]),
        output_commitment1: u256(env, &public[6]),
        public_amount: u256(env, &public[1]),
        ext_data_hash: soroban_sdk::BytesN::from_array(env, &public[2]),
        asp_membership_root: u256(env, &public[7]),
    }
}

/// A dummy input: worth nothing, fresh blinding so its nullifier is new.
fn dummy(private_key: NoteScalar, seed: u8) -> SpentNote {
    SpentNote {
        private_key,
        blinding: seeded_scalar(seed),
        amount: 0,
        leaf_index: 0,
        path_elements: vec![NoteScalar::from(0u64); TREE_LEVELS],
    }
}

fn rng(seed: u8) -> ark_std::rand::rngs::StdRng {
    use ark_std::rand::SeedableRng;
    ark_std::rand::rngs::StdRng::from_seed([seed; 32])
}

#[test]
fn shield_then_pay_privately_through_the_real_contracts() {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
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
            U256::from_u32(&env, 1_000_000_000),
            admin.clone(),
            0u32,
            LEVELS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);
    let balance = TokenClient::new(&env, &token);

    // Alice: a funded account, a note key on the allowlist, an encryption key.
    let alice = Address::generate(&env);
    StellarAssetClient::new(&env, &token).mint(&alice, &1_000);
    let alice_key = seeded_scalar(11);
    let alice_public = note_public_key(alice_key);
    let alice_secret = StaticSecret::from([12u8; 32]);
    let alice_encryption = PublicKey::from(&alice_secret).to_bytes();
    // Bob only receives, so he needs no allowlist entry.
    let bob_key = seeded_scalar(21);
    let bob_public = note_public_key(bob_key);
    let bob_secret = StaticSecret::from([22u8; 32]);
    let bob_encryption = PublicKey::from(&bob_secret).to_bytes();

    let alice_leaf = membership_leaf(alice_public);
    asp.insert_leaf(&u256(&env, &scalar_to_be_bytes(alice_leaf)));
    // Alice's membership path, as the API computes it.
    let membership_proof = path(LEVELS, &[(0, field(alice_leaf))], 0)
        .expect("path")
        .expect("leaf");
    assert_eq!(
        u256(&env, membership_proof.root.to_u256().as_bytes()),
        asp.get_root(),
        "the API's allowlist path leads to the contract's root"
    );
    let membership = Membership {
        path_elements: membership_proof
            .siblings
            .iter()
            .copied()
            .map(scalar)
            .collect(),
        path_indices: membership_proof.path_indices(),
        root: scalar(membership_proof.root),
    };

    // 1. Shield 40 into a note for Alice.
    let shield_note = CreatedNote {
        public_key: alice_public,
        blinding: seeded_scalar(31),
        amount: 40,
    };
    let zero_note = CreatedNote {
        public_key: alice_public,
        blinding: seeded_scalar(32),
        amount: 0,
    };
    let opened = Opened {
        amount: amount_bytes(40),
        blinding: scalar_to_be_bytes(shield_note.blinding),
    };
    let ext = ExtData {
        recipient: strkey(&alice),
        ext_amount: 40,
        encrypted_output0: note_crypto::encrypt(
            &alice_encryption,
            StaticSecret::from([33u8; 32]),
            &opened,
        )
        .expect("encrypts"),
        encrypted_output1: note_crypto::encrypt(
            &alice_encryption,
            StaticSecret::from([34u8; 32]),
            &Opened {
                amount: amount_bytes(0),
                blinding: scalar_to_be_bytes(zero_note.blinding),
            },
        )
        .expect("encrypts"),
    };
    let pool_ext = pool_ext_data(&env, &ext);
    assert_eq!(
        ext.hash().expect("hashes"),
        hash_ext_data(&env, &pool_ext).to_array(),
        "the off-chain external data hash is the pool's"
    );
    let shield = Transaction {
        root: scalar_from_be_bytes(
            &pool
                .get_root()
                .to_be_bytes()
                .to_buffer::<32>()
                .as_slice()
                .try_into()
                .expect("32"),
        ),
        ext_amount: 40,
        ext_data_hash: ext.hash().expect("hashes"),
        inputs: [dummy(alice_key, 41), dummy(alice_key, 42)],
        memberships: [membership.clone(), membership.clone()],
        outputs: [shield_note, zero_note],
    };
    let fixture = prove_transaction(&shield, &mut rng(1)).expect("proves the shield");
    pool.transact(&soroban_proof(&env, &fixture), &pool_ext, &alice);
    assert_eq!(balance.balance(&pool_id), 40);
    assert_eq!(balance.balance(&alice), 960);

    // Alice finds her note by opening it.
    let found = note_crypto::decrypt(&alice_secret, &ext.encrypted_output0).expect("opens");
    assert_eq!(found, opened);

    // 2. Pay Bob 15 privately, 25 back to Alice. The shield's outputs sit at
    // positions 0 and 1; the path comes from the API's tree code.
    let leaves = [
        (
            0,
            field(note_commitment(
                NoteScalar::from(40u64),
                alice_public,
                shield_note.blinding,
            )),
        ),
        (
            1,
            field(note_commitment(
                NoteScalar::from(0u64),
                alice_public,
                zero_note.blinding,
            )),
        ),
    ];
    let spend_path = path(LEVELS, &leaves, 0).expect("path").expect("leaf");
    assert_eq!(
        u256(&env, spend_path.root.to_u256().as_bytes()),
        pool.get_root(),
        "the API's pool path leads to the pool's root"
    );
    let spent = SpentNote {
        private_key: alice_key,
        blinding: shield_note.blinding,
        amount: 40,
        leaf_index: 0,
        path_elements: spend_path.siblings.iter().copied().map(scalar).collect(),
    };
    let to_bob = CreatedNote {
        public_key: bob_public,
        blinding: seeded_scalar(51),
        amount: 15,
    };
    let change = CreatedNote {
        public_key: alice_public,
        blinding: seeded_scalar(52),
        amount: 25,
    };
    let payment = ExtData {
        recipient: strkey(&alice),
        ext_amount: 0,
        encrypted_output0: note_crypto::encrypt(
            &bob_encryption,
            StaticSecret::from([53u8; 32]),
            &Opened {
                amount: amount_bytes(15),
                blinding: scalar_to_be_bytes(to_bob.blinding),
            },
        )
        .expect("encrypts"),
        encrypted_output1: note_crypto::encrypt(
            &alice_encryption,
            StaticSecret::from([54u8; 32]),
            &Opened {
                amount: amount_bytes(25),
                blinding: scalar_to_be_bytes(change.blinding),
            },
        )
        .expect("encrypts"),
    };
    let pay = Transaction {
        root: scalar(spend_path.root),
        ext_amount: 0,
        ext_data_hash: payment.hash().expect("hashes"),
        inputs: [spent, dummy(alice_key, 55)],
        memberships: [membership.clone(), membership],
        outputs: [to_bob, change],
    };
    let fixture = prove_transaction(&pay, &mut rng(2)).expect("proves the payment");
    let nullifier = note_nullifier(
        alice_key,
        note_commitment(NoteScalar::from(40u64), alice_public, shield_note.blinding),
        0,
    );
    assert_eq!(fixture.public_inputs_be()[3], scalar_to_be_bytes(nullifier));
    pool.transact(
        &soroban_proof(&env, &fixture),
        &pool_ext_data(&env, &payment),
        &alice,
    );

    // Nothing moved publicly, and Alice's note is spent.
    assert_eq!(balance.balance(&pool_id), 40);
    assert!(pool.has_nullifier(&u256(&env, &scalar_to_be_bytes(nullifier))));
    // Spending it again is refused.
    assert!(
        pool.try_transact(
            &soroban_proof(&env, &fixture),
            &pool_ext_data(&env, &payment),
            &alice
        )
        .is_err()
    );

    // Bob opens his note and it matches the commitment the pool inserted.
    let bob_opened =
        note_crypto::decrypt(&bob_secret, &payment.encrypted_output0).expect("Bob opens it");
    assert_eq!(bob_opened.amount, amount_bytes(15));
    let bob_commitment = note_commitment(
        NoteScalar::from(15u64),
        bob_public,
        scalar_from_be_bytes(&bob_opened.blinding),
    );
    assert!(pool.has_commitment(&u256(&env, &scalar_to_be_bytes(bob_commitment))));
    // Alice cannot open Bob's note.
    assert_eq!(
        note_crypto::decrypt(&alice_secret, &payment.encrypted_output0),
        None
    );
}
