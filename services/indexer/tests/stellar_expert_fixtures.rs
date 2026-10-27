//! The decoder against real testnet history.
//!
//! The fixtures are the complete event listings Stellar Expert returned on
//! 2026-10-24 for the deployment in `deployments/testnet.json`:
//!
//! - `pool_CDJELV6H.json`: the live pool
//! - `asp_CBRZ7RD5.json`: the allowlist the live pool reads
//! - `asp_revocation_CBYUTPDE.json`: the throwaway allowlist the revocation
//!   run used, the only place a `LeafRevoked` exists on chain
//!
//! Besides decoding every record, these tests check the invariants the
//! indexer will rely on: ledger order, unique tree positions, and settlements
//! whose outputs match the commitments inserted in the same operation.

use std::collections::{BTreeMap, BTreeSet};

use veil_indexer::event::{Decoded, U256Be, VeilEvent};
use veil_indexer::position::EventPosition;
use veil_indexer::source::{ContractEvent, ExpertEventRecord, Source};

const POOL: &str = "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G";
const ALLOWLIST: &str = "CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB";
const TOKEN: &str = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC";

fn load(name: &str) -> Vec<(ContractEvent, Decoded)> {
    let path = format!(
        "{}/tests/fixtures/stellar_expert/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("fixture exists");
    let records: Vec<ExpertEventRecord> = serde_json::from_str(&text).expect("fixture is JSON");
    records
        .into_iter()
        .map(|record| {
            let event = record.into_event().expect("record parses");
            let decoded = event.raw.decode().expect("every recorded event decodes");
            (event, decoded)
        })
        .collect()
}

fn events(listing: &[(ContractEvent, Decoded)]) -> Vec<(EventPosition, &VeilEvent)> {
    listing
        .iter()
        .filter_map(|(event, decoded)| match decoded {
            Decoded::Event(veil) => Some((event.position, veil)),
            Decoded::Ignored { .. } => None,
        })
        .collect()
}

fn count(listing: &[(ContractEvent, Decoded)]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for (_, decoded) in listing {
        let name = match decoded {
            Decoded::Event(event) => event.name(),
            Decoded::Ignored { .. } => "ignored",
        };
        let entry = counts.entry(name).or_insert(0usize);
        *entry = entry.saturating_add(1);
    }
    counts
}

#[test]
fn every_record_is_in_ledger_order_and_from_the_expected_contract() {
    for (name, contract) in [
        ("pool_CDJELV6H.json", POOL),
        ("asp_CBRZ7RD5.json", ALLOWLIST),
        (
            "asp_revocation_CBYUTPDE.json",
            "CBYUTPDEXTMK6DD53FNY4KHPGY3F6MZFVM27LSRRZJUKBADAZEI6FGBL",
        ),
    ] {
        let listing = load(name);
        assert!(!listing.is_empty(), "{name} is empty");
        assert!(
            listing
                .windows(2)
                .all(|pair| pair[0].0.position < pair[1].0.position),
            "{name} is not in strictly increasing position order"
        );
        for (event, _) in &listing {
            assert_eq!(event.contract, contract, "{name}");
            assert_eq!(event.source, Source::StellarExpert);
        }
    }
}

#[test]
fn the_live_pool_history_decodes_completely() {
    let listing = load("pool_CDJELV6H.json");
    let counts = count(&listing);
    assert_eq!(counts.get("new_commitment_event"), Some(&20));
    assert_eq!(counts.get("new_nullifier_event"), Some(&20));
    assert_eq!(counts.get("Settlement"), Some(&20));
    assert_eq!(counts.get("Deposit"), Some(&7));
    // Two wasm upgrades, which the host reports under the pool's ID.
    assert_eq!(counts.get("ignored"), Some(&2));
    assert_eq!(counts.values().sum::<usize>(), listing.len());
}

#[test]
fn tree_positions_are_never_reused() {
    let listing = load("pool_CDJELV6H.json");
    let mut seen = BTreeSet::new();
    for (position, event) in events(&listing) {
        let index = match event {
            VeilEvent::NewCommitment { index, .. } => *index,
            VeilEvent::LegacyDeposit(deposit) => deposit.index,
            _ => continue,
        };
        assert!(
            seen.insert(index),
            "tree index {index} inserted twice, again at {position}"
        );
    }
}

#[test]
fn settlements_match_the_commitments_their_operation_inserted() {
    let listing = load("pool_CDJELV6H.json");
    let decoded = events(&listing);

    // Commitments inserted by each operation, keyed by TOID.
    let mut inserted: BTreeMap<u64, BTreeMap<u32, U256Be>> = BTreeMap::new();
    for (position, event) in &decoded {
        if let VeilEvent::NewCommitment {
            commitment, index, ..
        } = event
        {
            inserted
                .entry(position.toid())
                .or_default()
                .insert(*index, *commitment);
        }
    }

    let mut settlements = 0usize;
    for (position, event) in &decoded {
        let VeilEvent::Settlement(settlement) = event else {
            continue;
        };
        settlements = settlements.saturating_add(1);
        assert_eq!(settlement.pool, POOL);
        assert_eq!(settlement.asset, TOKEN);
        let outputs = inserted
            .get(&position.toid())
            .unwrap_or_else(|| panic!("no commitments in the operation at {position}"));
        assert_eq!(
            outputs.get(&settlement.output_index0),
            Some(&settlement.output_commitment0)
        );
        assert_eq!(
            outputs.get(&settlement.output_index1),
            Some(&settlement.output_commitment1)
        );
    }
    assert_eq!(settlements, 20);
}

#[test]
fn every_nullifier_is_spent_once_and_settled() {
    let listing = load("pool_CDJELV6H.json");
    let decoded = events(&listing);
    let mut spent = BTreeSet::new();
    let mut settled = BTreeSet::new();
    for (_, event) in &decoded {
        match event {
            VeilEvent::NewNullifier { nullifier } => {
                assert!(
                    spent.insert(*nullifier),
                    "nullifier {nullifier} spent twice"
                );
            }
            VeilEvent::Settlement(settlement) => {
                settled.insert(settlement.nullifier);
            }
            _ => {}
        }
    }
    assert_eq!(spent, settled);
}

#[test]
fn the_allowlist_history_decodes_completely() {
    let listing = load("asp_CBRZ7RD5.json");
    assert_eq!(count(&listing).get("LeafAdded"), Some(&listing.len()));
    let indices: Vec<u64> = events(&listing)
        .into_iter()
        .filter_map(|(_, event)| match event {
            VeilEvent::LeafAdded { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let expected: Vec<u64> = (0..indices.len() as u64).collect();
    assert_eq!(
        indices, expected,
        "allowlist leaves are not contiguous from 0"
    );
}

#[test]
fn a_revocation_on_chain_decodes() {
    let listing = load("asp_revocation_CBYUTPDE.json");
    let names: Vec<&str> = events(&listing)
        .into_iter()
        .map(|(_, event)| event.name())
        .collect();
    // insert_leaf, revoke_leaf, then publish_leaf on the operator-maintained tree.
    assert_eq!(names, ["LeafAdded", "LeafRevoked", "LeafAdded"]);
}
