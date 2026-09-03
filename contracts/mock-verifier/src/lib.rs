#![no_std]

//! Demo-only verifier fallback for local integration.
//!
//! This contract intentionally does not perform Groth16 verification. It
//! accepts one deterministic mock proof shape and the selected policy
//! transaction public-input length so the pool can be exercised end to end
//! while real proof fixtures/proving are still being wired.

pub use contract_types::{Groth16Error, Groth16Proof};
use soroban_sdk::{
    BytesN, Env, Vec, contract, contractimpl,
    crypto::bn254::{Bn254Fr, Bn254G1Affine as G1Affine, Bn254G2Affine as G2Affine},
};

/// Public input count for the 2-input/2-output policy transaction.
pub const MOCK_POLICY_PUBLIC_INPUTS: u32 = 9;

/// Demo-only verifier contract matching the real verifier's `verify` API.
#[contract]
pub struct MockGroth16Verifier;

#[contractimpl]
impl MockGroth16Verifier {
    /// Verify a deterministic mock proof.
    ///
    /// This is a fallback path for local demos. It rejects malformed public
    /// input length and any proof that does not match [`mock_proof`].
    pub fn verify(
        env: Env,
        proof: Groth16Proof,
        public_inputs: Vec<Bn254Fr>,
    ) -> Result<bool, Groth16Error> {
        verify_mock_policy_transaction(&env, &proof, &public_inputs)
    }
}

/// Build the deterministic proof accepted by [`MockGroth16Verifier`].
pub fn mock_proof(env: &Env) -> Groth16Proof {
    Groth16Proof {
        a: G1Affine::from_bytes(mock_g1_bytes(env)),
        b: G2Affine::from_bytes(mock_g2_bytes(env)),
        c: G1Affine::from_bytes(mock_g1_bytes(env)),
    }
}

/// Validate the deterministic mock proof and public-input shape.
pub fn verify_mock_policy_transaction(
    env: &Env,
    proof: &Groth16Proof,
    public_inputs: &Vec<Bn254Fr>,
) -> Result<bool, Groth16Error> {
    if public_inputs.len() != MOCK_POLICY_PUBLIC_INPUTS {
        return Err(Groth16Error::MalformedPublicInputs);
    }
    if proof.is_empty() {
        return Err(Groth16Error::MalformedProof);
    }

    let expected = mock_proof(env);
    if proof.a.to_bytes() != expected.a.to_bytes()
        || proof.b.to_bytes() != expected.b.to_bytes()
        || proof.c.to_bytes() != expected.c.to_bytes()
    {
        return Err(Groth16Error::InvalidProof);
    }

    Ok(true)
}

fn mock_g1_bytes(env: &Env) -> BytesN<64> {
    let mut bytes = [0u8; 64];
    bytes[31] = 1;
    bytes[63] = 2;
    BytesN::from_array(env, &bytes)
}

fn mock_g2_bytes(env: &Env) -> BytesN<128> {
    let mut bytes = [0u8; 128];
    bytes[31] = 1;
    bytes[63] = 1;
    bytes[95] = 1;
    bytes[127] = 1;
    BytesN::from_array(env, &bytes)
}

#[cfg(test)]
mod test;
