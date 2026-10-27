//! The decoder against the contracts' own event types.
//!
//! Each test publishes an event through the struct the contract itself uses
//! (`pool::event::*`, `asp_membership::event::*`) and decodes the XDR the host
//! records. If a field is added, removed, renamed or retyped in the contract
//! source, the matching test here fails before the indexer ever meets such an
//! event on chain. This also covers the events the live deployment has never
//! emitted, such as `Exit` and `public_key_event`.

use soroban_sdk::testutils::{Address as _, Events as _};
use soroban_sdk::xdr::{self, ContractEventBody};
use soroban_sdk::{Address, Bytes, Env, U256, contract, contractimpl};
use veil_indexer::event::{Decoded, Exit, LegacyDeposit, RawEvent, Settlement, U256Be, VeilEvent};

#[contract]
struct Emitter;

#[contractimpl]
impl Emitter {
    pub fn noop() {}
}

/// Publishes `event` from a registered contract and decodes what the host
/// recorded.
fn emit_and_decode(env: &Env, publish: impl FnOnce(&Env)) -> VeilEvent {
    let emitter = env.register(Emitter, ());
    env.as_contract(&emitter, || publish(env));

    let recorded = env.events().all().filter_by_contract(&emitter);
    let [event] = recorded.events() else {
        panic!("expected exactly one event, got {:?}", recorded.events());
    };
    let ContractEventBody::V0(body) = &event.body;
    let raw = RawEvent {
        topics: body.topics.to_vec(),
        data: body.data.clone(),
    };
    match raw.decode().expect("contract event decodes") {
        Decoded::Event(event) => event,
        Decoded::Ignored { name } => panic!("{name} was ignored"),
    }
}

fn u256(env: &Env, value: u32) -> U256 {
    U256::from_u32(env, value)
}

fn be(value: u32) -> U256Be {
    let mut bytes = [0u8; 32];
    bytes[28..].copy_from_slice(&value.to_be_bytes());
    U256Be(bytes)
}

fn strkey(address: &Address) -> String {
    let address = xdr::ScAddress::from(address);
    address.to_string()
}

#[test]
fn new_commitment() {
    let env = Env::default();
    let decoded = emit_and_decode(&env, |env| {
        pool::event::NewCommitmentEvent {
            commitment: u256(env, 11),
            index: 7,
            encrypted_output: Bytes::from_array(env, &[1, 2, 3]),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::NewCommitment {
            commitment: be(11),
            index: 7,
            encrypted_output: vec![1, 2, 3],
        }
    );
}

#[test]
fn new_nullifier() {
    let env = Env::default();
    let decoded = emit_and_decode(&env, |env| {
        pool::event::NewNullifierEvent {
            nullifier: u256(env, 12),
        }
        .publish(env);
    });
    assert_eq!(decoded, VeilEvent::NewNullifier { nullifier: be(12) });
}

#[test]
fn settlement_with_and_without_a_recipient() {
    let env = Env::default();
    let pool_id = Address::generate(&env);
    let asset = Address::generate(&env);
    let recipient = Address::generate(&env);

    for withdrawn_to in [None, Some(recipient.clone())] {
        let decoded = emit_and_decode(&env, |env| {
            pool::event::SettlementEvent {
                nullifier: u256(env, 1),
                pool: pool_id.clone(),
                output_commitment0: u256(env, 2),
                output_commitment1: u256(env, 3),
                output_index0: 4,
                output_index1: 5,
                amount_bucket: -6,
                public_amount: u256(env, 7),
                recipient: withdrawn_to.clone(),
                asset: asset.clone(),
            }
            .publish(env);
        });
        assert_eq!(
            decoded,
            VeilEvent::Settlement(Settlement {
                nullifier: be(1),
                pool: strkey(&pool_id),
                output_commitment0: be(2),
                output_commitment1: be(3),
                output_index0: 4,
                output_index1: 5,
                amount_bucket: -6,
                public_amount: be(7),
                recipient: withdrawn_to.as_ref().map(strkey),
                asset: strkey(&asset),
            })
        );
    }
}

#[test]
fn exit() {
    let env = Env::default();
    let pool_id = Address::generate(&env);
    let asset = Address::generate(&env);
    let recipient = Address::generate(&env);
    let decoded = emit_and_decode(&env, |env| {
        pool::event::ExitEvent {
            nullifier: u256(env, 21),
            pool: pool_id.clone(),
            amount_bucket: i128::MAX,
            public_amount: u256(env, 22),
            recipient: recipient.clone(),
            asset: asset.clone(),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::Exit(Exit {
            nullifier: be(21),
            pool: strkey(&pool_id),
            amount_bucket: i128::MAX,
            public_amount: be(22),
            recipient: strkey(&recipient),
            asset: strkey(&asset),
        })
    );
}

#[test]
fn legacy_deposit() {
    let env = Env::default();
    let pool_id = Address::generate(&env);
    let asset = Address::generate(&env);
    let decoded = emit_and_decode(&env, |env| {
        pool::event::DepositEvent {
            commitment: u256(env, 31),
            pool: pool_id.clone(),
            index: 2,
            amount_bucket: i128::MIN,
            asset: asset.clone(),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::LegacyDeposit(LegacyDeposit {
            commitment: be(31),
            pool: strkey(&pool_id),
            index: 2,
            amount_bucket: i128::MIN,
            asset: strkey(&asset),
        })
    );
}

#[test]
fn public_key() {
    let env = Env::default();
    let owner = Address::generate(&env);
    let decoded = emit_and_decode(&env, |env| {
        pool::event::PublicKeyEvent {
            owner: owner.clone(),
            encryption_key: Bytes::from_array(env, &[7u8; 32]),
            note_key: Bytes::from_array(env, &[9u8; 32]),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::PublicKey {
            owner: strkey(&owner),
            encryption_key: vec![7u8; 32],
            note_key: vec![9u8; 32],
        }
    );
}

#[test]
fn leaf_added() {
    let env = Env::default();
    let decoded = emit_and_decode(&env, |env| {
        asp_membership::event::LeafAddedEvent {
            leaf: u256(env, 41),
            index: u64::MAX,
            root: u256(env, 42),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::LeafAdded {
            leaf: be(41),
            index: u64::MAX,
            root: be(42),
        }
    );
}

#[test]
fn leaf_revoked() {
    let env = Env::default();
    let decoded = emit_and_decode(&env, |env| {
        asp_membership::event::LeafRevokedEvent {
            leaf: u256(env, 51),
            root: u256(env, 52),
        }
        .publish(env);
    });
    assert_eq!(
        decoded,
        VeilEvent::LeafRevoked {
            leaf: be(51),
            root: be(52),
        }
    );
}
