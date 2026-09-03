//! Verifier boundary for the policy transaction circuit.
//!
//! Keep the public-input construction isolated here so the pool can swap the
//! verifier implementation without spreading circuit ordering assumptions
//! through transaction logic.

use contract_types::{Groth16Error, Groth16Proof};
use soroban_sdk::{Address, BytesN, Env, U256, Vec, contractclient, crypto::bn254::Bn254Fr};

use crate::types::Proof;

/// Contract client for the Groth16 verifier contract.
#[contractclient(crate_path = "soroban_sdk", name = "CircomGroth16VerifierClient")]
pub trait CircomGroth16VerifierInterface {
    fn verify(
        env: Env,
        proof: Groth16Proof,
        public_inputs: Vec<Bn254Fr>,
    ) -> Result<bool, Groth16Error>;
}

/// Public inputs passed to the verifier contract.
///
/// The order is stable and documented for off-chain proof adapters:
/// 1. transaction Merkle root
/// 2. public amount
/// 3. external data hash
/// 4. input nullifiers, in proof order
/// 5. output commitment 0
/// 6. output commitment 1
/// 7. ASP membership root repeated once per input nullifier
///
/// For the 2-in/2-out instance that is 9 values, matching the circuit.
pub struct VerifierPublicInputs {
    pub values: Vec<Bn254Fr>,
}

impl VerifierPublicInputs {
    pub fn from_proof(env: &Env, proof: &Proof) -> Self {
        let mut values: Vec<Bn254Fr> = Vec::new(env);

        values.push_back(fr_from_u256(env, &proof.root));
        values.push_back(fr_from_u256(env, &proof.public_amount));
        values.push_back(Bn254Fr::from_bytes(proof.ext_data_hash.clone()));
        for nullifier in proof.input_nullifiers.iter() {
            values.push_back(fr_from_u256(env, &nullifier));
        }
        values.push_back(fr_from_u256(env, &proof.output_commitment0));
        values.push_back(fr_from_u256(env, &proof.output_commitment1));
        for _ in 0..proof.input_nullifiers.len() {
            values.push_back(fr_from_u256(env, &proof.asp_membership_root));
        }

        Self { values }
    }
}

/// Verify a policy transaction proof through the configured verifier contract.
pub fn verify_policy_transaction(env: &Env, verifier: &Address, proof: &Proof) -> bool {
    let client = CircomGroth16VerifierClient::new(env, verifier);
    let public_inputs = VerifierPublicInputs::from_proof(env, proof);

    client.verify(&proof.proof, &public_inputs.values)
}

fn fr_from_u256(env: &Env, value: &U256) -> Bn254Fr {
    let mut buf = [0u8; 32];
    value.to_be_bytes().copy_into_slice(&mut buf);
    Bn254Fr::from_bytes(BytesN::from_array(env, &buf))
}
