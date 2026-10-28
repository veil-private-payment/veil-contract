//! The off-chain trees against the contracts' own code and real history.
//!
//! `src/tree.rs` reimplements Poseidon2 compression and the zero hashes with
//! `zkhash`. If it drifted from the contracts by a single bit, every root the
//! indexer computes would be wrong, so each piece is compared with the
//! function the contracts call, run in a Soroban test environment.

use soroban_sdk::{Bytes, Env, U256, contract, contractimpl};
use veil_indexer::event::{Decoded, U256Be, VeilEvent};
use veil_indexer::source::ExpertEventRecord;
use veil_indexer::tree::{Field, IncrementalTree, compress, root, zeroes};

fn to_sdk(env: &Env, value: &U256Be) -> U256 {
    U256::from_be_bytes(env, &Bytes::from_array(env, value.as_bytes()))
}

fn from_sdk(value: &U256) -> U256Be {
    let bytes = value.to_be_bytes();
    let mut out = [0u8; 32];
    bytes.copy_into_slice(&mut out);
    U256Be(out)
}

/// A few field elements spread over the range, including the edges.
fn samples() -> Vec<U256Be> {
    let mut out = vec![U256Be([0; 32])];
    let mut one = [0u8; 32];
    one[31] = 1;
    out.push(U256Be(one));
    // p - 1 for BN254's scalar field.
    out.push(U256Be(hex_32(
        "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000000",
    )));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..8 {
        let mut bytes = [0u8; 32];
        for chunk in bytes.chunks_mut(8) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            chunk.copy_from_slice(&seed.to_be_bytes());
        }
        // Keep below the modulus.
        bytes[0] &= 0x1f;
        out.push(U256Be(bytes));
    }
    out
}

fn hex_32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let text = std::str::from_utf8(pair).expect("ascii");
        *slot = u8::from_str_radix(text, 16).expect("hex");
    }
    out
}

#[test]
fn the_zero_hashes_are_the_contracts_at_every_level() {
    let env = Env::default();
    let theirs = soroban_utils::get_zeroes(&env);
    let ours = zeroes(32).expect("zeroes");
    assert_eq!(theirs.len(), 33);
    for (level, value) in theirs.iter().enumerate() {
        assert_eq!(ours[level].to_u256(), from_sdk(&value), "level {level}");
    }
}

#[test]
fn compression_is_the_contracts() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    let values = samples();
    for left in &values {
        for right in &values {
            let theirs =
                soroban_utils::poseidon2_compress(&env, to_sdk(&env, left), to_sdk(&env, right));
            let ours = compress(
                Field::from_u256(left).expect("field element"),
                Field::from_u256(right).expect("field element"),
            );
            assert_eq!(ours.to_u256(), from_sdk(&theirs), "{left} {right}");
        }
    }
}

#[contract]
struct TreeHost;

#[contractimpl]
impl TreeHost {
    pub fn noop() {}
}

#[test]
fn the_pool_tree_root_is_the_contracts_after_every_pair() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    let host = env.register(TreeHost, ());
    let levels = 16;
    let values = samples();

    env.as_contract(&host, || {
        pool::merkle_with_history::MerkleTreeWithHistory::init(&env, levels).expect("init");
    });
    let mut leaves = Vec::new();
    for (pair, chunk) in values.chunks_exact(2).enumerate() {
        let (left, right) = (&chunk[0], &chunk[1]);
        let on_chain = env.as_contract(&host, || {
            pool::merkle_with_history::MerkleTreeWithHistory::insert_two_leaves(
                &env,
                to_sdk(&env, left),
                to_sdk(&env, right),
            )
            .expect("insert");
            pool::merkle_with_history::MerkleTreeWithHistory::get_last_root(&env).expect("root")
        });
        let index = u64::try_from(pair).expect("fits").saturating_mul(2);
        leaves.push((index, Field::from_u256(left).expect("field")));
        leaves.push((
            index.saturating_add(1),
            Field::from_u256(right).expect("field"),
        ));
        let ours = root(levels, &leaves).expect("root");
        assert_eq!(ours.to_u256(), from_sdk(&on_chain), "after pair {pair}");
    }
}

#[test]
fn the_allowlist_roots_on_testnet_are_reproduced_one_insertion_at_a_time() {
    let path = format!(
        "{}/tests/fixtures/stellar_expert/asp_CBRZ7RD5.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let records: Vec<ExpertEventRecord> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    let mut tree = IncrementalTree::new(16).expect("tree");
    let mut checked = 0;
    for record in records {
        let event = record.into_event().expect("parses");
        let Ok(Decoded::Event(VeilEvent::LeafAdded { leaf, index, root })) = event.raw.decode()
        else {
            panic!("the live allowlist only has additions");
        };
        assert_eq!(index, tree.next_index());
        let ours = tree
            .push(Field::from_u256(&leaf).expect("field"))
            .expect("room");
        assert_eq!(ours.to_u256(), root, "root after leaf {index}");
        checked += 1;
    }
    assert_eq!(checked, 5);
}
