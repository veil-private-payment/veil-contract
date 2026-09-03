use super::*;
use soroban_sdk::{BytesN, Env, Vec, crypto::bn254::Bn254Fr};

fn fr_from_u32(env: &Env, value: u32) -> Bn254Fr {
    let mut bytes = [0u8; 32];
    bytes[28..].copy_from_slice(&value.to_be_bytes());
    Bn254Fr::from_bytes(BytesN::from_array(env, &bytes))
}

fn policy_public_inputs(env: &Env) -> Vec<Bn254Fr> {
    let mut inputs = Vec::new(env);
    for i in 0..MOCK_POLICY_PUBLIC_INPUTS {
        inputs.push_back(fr_from_u32(env, i));
    }
    inputs
}

#[test]
fn accepts_deterministic_policy_fixture_shape() {
    let env = Env::default();
    let proof = mock_proof(&env);
    let inputs = policy_public_inputs(&env);

    let result = MockGroth16Verifier::verify(env, proof, inputs);

    assert_eq!(result, Ok(true));
}

#[test]
fn rejects_wrong_public_input_length() {
    let env = Env::default();
    let proof = mock_proof(&env);
    let mut inputs = Vec::new(&env);
    inputs.push_back(fr_from_u32(&env, 1));

    let result = MockGroth16Verifier::verify(env, proof, inputs);

    assert_eq!(result, Err(Groth16Error::MalformedPublicInputs));
}

#[test]
fn rejects_unknown_proof_fixture() {
    let env = Env::default();
    let mut proof = mock_proof(&env);
    let mut wrong_a = [0u8; 64];
    wrong_a[31] = 2;
    wrong_a[63] = 2;
    proof.a = G1Affine::from_bytes(BytesN::from_array(&env, &wrong_a));
    let inputs = policy_public_inputs(&env);

    let result = MockGroth16Verifier::verify(env, proof, inputs);

    assert_eq!(result, Err(Groth16Error::InvalidProof));
}
