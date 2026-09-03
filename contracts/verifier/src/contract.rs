use soroban_sdk::{Env, Vec, contract, contractimpl, crypto::bn254::Bn254Fr, vec};

use contract_types::{Groth16Error, Groth16Proof};

use crate::verification_key::{VerificationKey, embedded_vk};

/// Groth16 verifier for BN254/Circom proofs.
#[contract]
pub struct CircomGroth16Verifier;

#[contractimpl]
impl CircomGroth16Verifier {
    /// Verify a Groth16 proof using the compile-time embedded verification key.
    pub fn verify(
        env: Env,
        proof: Groth16Proof,
        public_inputs: Vec<Bn254Fr>,
    ) -> Result<bool, Groth16Error> {
        let vk = embedded_vk(&env);
        Self::verify_with_vk(&env, &vk, proof, public_inputs)
    }

    pub(crate) fn verify_with_vk(
        env: &Env,
        vk: &VerificationKey,
        proof: Groth16Proof,
        pub_inputs: Vec<Bn254Fr>,
    ) -> Result<bool, Groth16Error> {
        let bn = env.crypto().bn254();

        if pub_inputs.len().checked_add(1) != Some(vk.ic.len()) {
            return Err(Groth16Error::MalformedPublicInputs);
        }

        let mut vk_x = vk.ic.get(0).ok_or(Groth16Error::MalformedPublicInputs)?;

        for i in 0..pub_inputs.len() {
            let s = pub_inputs
                .get(i)
                .ok_or(Groth16Error::MalformedPublicInputs)?;
            let ic_idx = i
                .checked_add(1)
                .ok_or(Groth16Error::MalformedPublicInputs)?;
            let v = vk
                .ic
                .get(ic_idx)
                .ok_or(Groth16Error::MalformedPublicInputs)?;
            let prod = bn.g1_mul(&v, &s);
            vk_x = bn.g1_add(&vk_x, &prod);
        }

        // Compute the pairing check:
        // e(-A, B) * e(alpha, beta) * e(vk_x, gamma) * e(C, delta) == 1
        #[allow(clippy::arithmetic_side_effects)]
        let neg_a = -proof.a;

        let g1_points = vec![env, neg_a, vk.alpha.clone(), vk_x, proof.c];
        let g2_points = vec![
            env,
            proof.b,
            vk.beta.clone(),
            vk.gamma.clone(),
            vk.delta.clone(),
        ];
        if bn.pairing_check(g1_points, g2_points) {
            Ok(true)
        } else {
            Err(Groth16Error::InvalidProof)
        }
    }
}
