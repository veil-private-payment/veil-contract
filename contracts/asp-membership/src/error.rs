use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Caller is not authorized to perform this operation
    NotAuthorized = 1,
    /// Merkle tree has reached maximum capacity
    MerkleTreeFull = 2,
    /// Wrong Number of levels specified
    WrongLevels = 3,
    /// The contract has not been yet initialized
    NotInitialized = 4,
    /// Arithmetic overflow occurred
    Overflow = 5,
    /// Field input is outside the BN254 scalar field
    InvalidFieldElement = 6,
}
