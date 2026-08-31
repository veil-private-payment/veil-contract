use crate::merkle_with_history::Error as MerkleError;
use soroban_sdk::contracterror;

/// Contract error types for the privacy pool
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// Caller is not authorized to perform this operation
    NotAuthorized = 1,
    /// Merkle tree has reached maximum capacity
    MerkleTreeFull = 2,
    /// Contract has already been initialized
    AlreadyInitialized = 3,
    /// Invalid Merkle tree levels configuration
    WrongLevels = 4,
    /// Internal error: next leaf index is not even
    NextIndexNotEven = 5,
    /// External amount is invalid (negative or exceeds 2^248)
    WrongExtAmount = 6,
    /// Zero-knowledge proof verification failed or proof is empty
    InvalidProof = 7,
    /// Provided Merkle root is not in the recent history
    UnknownRoot = 8,
    /// Nullifier has already been spent (double-spend attempt)
    AlreadySpentNullifier = 9,
    /// External data hash does not match the provided data
    WrongExtHash = 10,
    /// Contract is not initialized
    NotInitialized = 11,
    /// Arithmetic overflow occurred
    Overflow = 12,
    /// Fee basis points are outside the allowed 0-10_000 range
    InvalidFee = 13,
    /// Commitment has already been inserted into the pool
    AlreadyInsertedCommitment = 14,
    /// A proof, nullifier, commitment, root, or ASP value is outside BN254
    InvalidFieldElement = 15,
}

/// Conversion from MerkleTreeWithHistory errors to pool contract errors
/// Errors from MerkleTreeWithHistory are not `contracterror`
impl From<MerkleError> for ContractError {
    fn from(e: MerkleError) -> Self {
        match e {
            MerkleError::AlreadyInitialized => ContractError::AlreadyInitialized,
            MerkleError::MerkleTreeFull => ContractError::MerkleTreeFull,
            MerkleError::WrongLevels => ContractError::WrongLevels,
            MerkleError::NextIndexNotEven => ContractError::NextIndexNotEven,
            MerkleError::NotInitialized => ContractError::NotInitialized,
            MerkleError::Overflow => ContractError::Overflow,
        }
    }
}
