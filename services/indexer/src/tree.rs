//! The Merkle trees the pool and the allowlist keep, recomputed off chain.
//!
//! Both contracts hash with Poseidon2 compression over BN254
//! (`soroban_utils::poseidon2_compress`) and fill empty positions with the
//! zero hashes of `soroban_utils::get_zeroes`: level 0 is a fixed empty leaf,
//! each level above is the compression of two of the level below. The tests in
//! `tests/tree_parity.rs` check both against the contract code itself.

use std::collections::BTreeMap;
use std::ops::AddAssign;

use ark_ff::{BigInteger, PrimeField};
use zkhash::fields::bn256::FpBN256 as Fr;
use zkhash::poseidon2::poseidon2::Poseidon2;
use zkhash::poseidon2::poseidon2_instance_bn256::POSEIDON2_BN256_PARAMS_2;

use crate::event::U256Be;

/// The value the contracts give an empty leaf, `get_zeroes()[0]`.
const EMPTY_LEAF: [u8; 32] = [
    37, 48, 34, 136, 219, 153, 53, 3, 68, 151, 65, 131, 206, 49, 13, 99, 181, 58, 187, 158, 240,
    248, 87, 87, 83, 238, 211, 110, 1, 24, 249, 206,
];

/// Deepest tree either contract accepts.
pub const MAX_LEVELS: u32 = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TreeError {
    #[error("{0} is not below the BN254 field modulus")]
    NotAFieldElement(U256Be),
    #[error("tree depth {0} is outside 1..={MAX_LEVELS}")]
    Levels(u32),
    #[error("leaf index {index} does not fit a tree of depth {levels}")]
    IndexOutOfRange { index: u64, levels: u32 },
    #[error("leaf index {0} is set twice")]
    DuplicateIndex(u64),
}

/// A BN254 field element, as the contracts hash it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field(Fr);

impl Field {
    /// Reads a big-endian value, refusing anything at or above the modulus,
    /// as the contracts do.
    pub fn from_u256(value: &U256Be) -> Result<Self, TreeError> {
        let element = Fr::from_be_bytes_mod_order(value.as_bytes());
        let field = Self(element);
        if field.to_u256() != *value {
            return Err(TreeError::NotAFieldElement(*value));
        }
        Ok(field)
    }

    pub fn zero() -> Self {
        Self(Fr::from(0u64))
    }

    pub fn to_u256(self) -> U256Be {
        let bytes = self.0.into_bigint().to_bytes_be();
        let mut out = [0u8; 32];
        // Right-align: the big integer is 32 bytes, but never rely on it.
        for (slot, byte) in out.iter_mut().rev().zip(bytes.iter().rev()) {
            *slot = *byte;
        }
        U256Be(out)
    }
}

/// Poseidon2 compression of two field elements: the first output of the
/// width-2 permutation plus the left input.
pub fn compress(left: Field, right: Field) -> Field {
    let hasher = Poseidon2::new(&POSEIDON2_BN256_PARAMS_2);
    let permuted = hasher.permutation(&[left.0, right.0]);
    let mut out = permuted.first().copied().unwrap_or_default();
    out.add_assign(&left.0);
    Field(out)
}

/// The empty subtree hash at each level, `0..=levels`.
pub fn zeroes(levels: u32) -> Result<Vec<Field>, TreeError> {
    check_levels(levels)?;
    let mut out = Vec::with_capacity(usize::try_from(levels).unwrap_or(0).saturating_add(1));
    let mut current = Field::from_u256(&U256Be(EMPTY_LEAF))?;
    out.push(current);
    for _ in 0..levels {
        current = compress(current, current);
        out.push(current);
    }
    Ok(out)
}

fn check_levels(levels: u32) -> Result<(), TreeError> {
    if levels == 0 || levels > MAX_LEVELS {
        return Err(TreeError::Levels(levels));
    }
    Ok(())
}

/// Root of a tree of depth `levels` with `leaves` set at their indices and
/// every other position empty.
///
/// Only the populated paths are hashed, so the cost follows the number of
/// leaves rather than the tree's capacity.
pub fn root(levels: u32, leaves: &[(u64, Field)]) -> Result<Field, TreeError> {
    let zeroes = zeroes(levels)?;
    let capacity = 1u64.checked_shl(levels).unwrap_or(u64::MAX);
    let mut nodes: BTreeMap<u64, Field> = BTreeMap::new();
    for &(index, leaf) in leaves {
        if index >= capacity {
            return Err(TreeError::IndexOutOfRange { index, levels });
        }
        if nodes.insert(index, leaf).is_some() {
            return Err(TreeError::DuplicateIndex(index));
        }
    }

    for zero in zeroes.iter().take(usize::try_from(levels).unwrap_or(0)) {
        let mut parents = BTreeMap::new();
        for &index in nodes.keys() {
            let parent = index >> 1;
            if parents.contains_key(&parent) {
                continue;
            }
            let left = nodes.get(&(parent << 1)).copied().unwrap_or(*zero);
            let right = nodes.get(&((parent << 1) | 1)).copied().unwrap_or(*zero);
            parents.insert(parent, compress(left, right));
        }
        nodes = parents;
    }
    let top = zeroes.last().copied().unwrap_or_else(Field::zero);
    Ok(nodes.get(&0).copied().unwrap_or(top))
}

/// A Merkle proof for one leaf: the sibling at each level from the leaf up,
/// and the root they lead to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MerklePath {
    pub index: u64,
    pub leaf: Field,
    pub siblings: Vec<Field>,
    pub root: Field,
}

impl MerklePath {
    /// Bit `i` set means the node at level `i` is a right child, the order the
    /// circuit's `pathIndices` uses.
    pub fn path_indices(&self) -> u64 {
        self.index
    }

    /// Recomputes the root from the leaf and siblings.
    pub fn computed_root(&self) -> Field {
        let mut index = self.index;
        let mut current = self.leaf;
        for sibling in &self.siblings {
            current = if index & 1 == 1 {
                compress(*sibling, current)
            } else {
                compress(current, *sibling)
            };
            index >>= 1;
        }
        current
    }
}

/// The proof for the leaf at `index` in a tree of depth `levels` holding
/// `leaves`, or `None` if no leaf is set at `index`.
pub fn path(
    levels: u32,
    leaves: &[(u64, Field)],
    index: u64,
) -> Result<Option<MerklePath>, TreeError> {
    let zeroes = zeroes(levels)?;
    let capacity = 1u64.checked_shl(levels).unwrap_or(u64::MAX);
    let mut nodes: BTreeMap<u64, Field> = BTreeMap::new();
    for &(at, leaf) in leaves {
        if at >= capacity {
            return Err(TreeError::IndexOutOfRange { index: at, levels });
        }
        if nodes.insert(at, leaf).is_some() {
            return Err(TreeError::DuplicateIndex(at));
        }
    }
    let Some(leaf) = nodes.get(&index).copied() else {
        return Ok(None);
    };

    let mut siblings = Vec::with_capacity(usize::try_from(levels).unwrap_or(0));
    let mut position = index;
    for zero in zeroes.iter().take(usize::try_from(levels).unwrap_or(0)) {
        siblings.push(nodes.get(&(position ^ 1)).copied().unwrap_or(*zero));
        let mut parents = BTreeMap::new();
        for &at in nodes.keys() {
            let parent = at >> 1;
            if parents.contains_key(&parent) {
                continue;
            }
            let left = nodes.get(&(parent << 1)).copied().unwrap_or(*zero);
            let right = nodes.get(&((parent << 1) | 1)).copied().unwrap_or(*zero);
            parents.insert(parent, compress(left, right));
        }
        nodes = parents;
        position >>= 1;
    }
    let top = zeroes.last().copied().unwrap_or_else(Field::zero);
    Ok(Some(MerklePath {
        index,
        leaf,
        siblings,
        root: nodes.get(&0).copied().unwrap_or(top),
    }))
}

/// An append-only tree that reports its root after every insertion, the way
/// the allowlist contract's `insert_leaf` computes it.
#[derive(Clone, Debug)]
pub struct IncrementalTree {
    levels: u32,
    zeroes: Vec<Field>,
    filled: Vec<Field>,
    next_index: u64,
    root: Field,
}

impl IncrementalTree {
    pub fn new(levels: u32) -> Result<Self, TreeError> {
        let zeroes = zeroes(levels)?;
        let root = zeroes.last().copied().unwrap_or_else(Field::zero);
        Ok(Self {
            levels,
            filled: zeroes.clone(),
            zeroes,
            next_index: 0,
            root,
        })
    }

    pub fn next_index(&self) -> u64 {
        self.next_index
    }

    pub fn root(&self) -> Field {
        self.root
    }

    /// Appends `leaf` and returns the new root.
    pub fn push(&mut self, leaf: Field) -> Result<Field, TreeError> {
        let capacity = 1u64.checked_shl(self.levels).unwrap_or(u64::MAX);
        if self.next_index >= capacity {
            return Err(TreeError::IndexOutOfRange {
                index: self.next_index,
                levels: self.levels,
            });
        }
        let mut index = self.next_index;
        let mut current = leaf;
        for (filled, zero) in self
            .filled
            .iter_mut()
            .zip(&self.zeroes)
            .take(usize::try_from(self.levels).unwrap_or(0))
        {
            if index & 1 == 1 {
                current = compress(*filled, current);
            } else {
                *filled = current;
                current = compress(current, *zero);
            }
            index >>= 1;
        }
        self.next_index = self.next_index.saturating_add(1);
        self.root = current;
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: u8) -> Field {
        let mut bytes = [0u8; 32];
        bytes[31] = value;
        Field::from_u256(&U256Be(bytes)).expect("small values are field elements")
    }

    #[test]
    fn values_at_or_above_the_modulus_are_refused() {
        assert!(Field::from_u256(&U256Be([0xff; 32])).is_err());
        let round_trip = field(7).to_u256();
        assert_eq!(Field::from_u256(&round_trip), Ok(field(7)));
    }

    #[test]
    fn an_empty_tree_has_the_top_zero_as_its_root() {
        let zeroes = zeroes(16).expect("zeroes");
        assert_eq!(root(16, &[]), Ok(zeroes[16]));
    }

    #[test]
    fn the_sparse_root_matches_hashing_every_position() {
        let levels = 4;
        let zeroes = zeroes(levels).expect("zeroes");
        let leaves = [(0, field(1)), (1, field(2)), (5, field(3)), (15, field(4))];
        let mut row = vec![zeroes[0]; 16];
        for &(index, leaf) in &leaves {
            row[usize::try_from(index).expect("small")] = leaf;
        }
        while row.len() > 1 {
            row = row
                .chunks_exact(2)
                .map(|pair| compress(pair[0], pair[1]))
                .collect();
        }
        assert_eq!(root(levels, &leaves), Ok(row[0]));
    }

    #[test]
    fn the_incremental_root_matches_the_sparse_root_after_every_insertion() {
        let mut tree = IncrementalTree::new(5).expect("tree");
        let mut leaves = Vec::new();
        for value in 1..=9u8 {
            leaves.push((u64::from(value).saturating_sub(1), field(value)));
            let incremental = tree.push(field(value)).expect("room");
            assert_eq!(Ok(incremental), root(5, &leaves));
        }
        assert_eq!(tree.next_index(), 9);
    }

    #[test]
    fn every_path_leads_to_the_root() {
        let levels = 5;
        let leaves: Vec<(u64, Field)> = [0u64, 1, 2, 7, 8, 30, 31]
            .iter()
            .map(|&i| (i, field(u8::try_from(i).expect("small").saturating_add(1))))
            .collect();
        let expected = root(levels, &leaves).expect("root");
        for &(index, leaf) in &leaves {
            let proof = path(levels, &leaves, index)
                .expect("path")
                .expect("leaf exists");
            assert_eq!(proof.leaf, leaf);
            assert_eq!(proof.siblings.len(), 5);
            assert_eq!(proof.root, expected);
            assert_eq!(proof.computed_root(), expected, "leaf {index}");
        }
        assert_eq!(path(levels, &leaves, 3).expect("path"), None);
    }

    #[test]
    fn bad_shapes_are_refused() {
        assert_eq!(root(0, &[]), Err(TreeError::Levels(0)));
        assert_eq!(root(33, &[]), Err(TreeError::Levels(33)));
        assert_eq!(
            root(2, &[(4, field(1))]),
            Err(TreeError::IndexOutOfRange {
                index: 4,
                levels: 2
            })
        );
        assert_eq!(
            root(2, &[(1, field(1)), (1, field(2))]),
            Err(TreeError::DuplicateIndex(1))
        );
        let mut full = IncrementalTree::new(1).expect("tree");
        full.push(field(1)).expect("room");
        full.push(field(2)).expect("room");
        assert!(full.push(field(3)).is_err());
    }
}
