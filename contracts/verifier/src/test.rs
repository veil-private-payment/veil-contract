use super::*;
use ark_bn254::{Bn254, Fr as ArkFr};
use ark_ff::{BigInteger, Field, PrimeField};
use ark_groth16::{Groth16, Proof};
use ark_relations::gr1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError, Variable};
use ark_std::rand::{SeedableRng, rngs::StdRng};
use contract_types::PROOF_SIZE;
use serde_json::Value;
use soroban_sdk::{
    Bytes, BytesN, Env, Vec,
    crypto::bn254::{Bn254Fr, Bn254G1Affine as G1Affine, Bn254G2Affine as G2Affine},
};
use soroban_utils::{g1_bytes_from_ark, g2_bytes_from_ark, vk_bytes_from_ark};

/// Simple circuit that exposes nine public inputs (as many as the policy circuit).
///
/// The circuit ties the first public input to a witness to keep the proving
/// key minimal while still exercising the fixed `ic` array length expected by
/// the contract (9 public inputs + 1 constant term).
#[derive(Clone)]
struct NineInputCircuit<F: Field> {
    inputs: [F; 9],
}

impl<F: Field> ConstraintSynthesizer<F> for NineInputCircuit<F> {
    fn generate_constraints(self, cs: ConstraintSystemRef<F>) -> Result<(), SynthesisError> {
        // Register all public inputs
        let mut input_vars = alloc::vec::Vec::with_capacity(self.inputs.len());
        for value in self.inputs {
            input_vars.push(cs.new_input_variable(|| Ok(value))?);
        }

        // Constrain a witness to equal the first public input: w * 1 = input_0
        let witness = cs.new_witness_variable(|| Ok(self.inputs[0]))?;
        let a_lc = witness.into();
        let b_lc = Variable::One.into();
        let c_lc = input_vars[0].into();
        cs.enforce_r1cs_constraint(|| a_lc, || b_lc, || c_lc)?;
        Ok(())
    }
}

fn seeded_rng() -> StdRng {
    StdRng::seed_from_u64(7)
}

fn fr_from_ark(env: &Env, value: ArkFr) -> Bn254Fr {
    let bytes = value.into_bigint().to_bytes_be();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&bytes);
    Bn254Fr::from_bytes(BytesN::from_array(env, &buf))
}

fn groth16_proof_from_ark(env: &Env, proof: &Proof<Bn254>) -> Groth16Proof {
    Groth16Proof {
        a: G1Affine::from_bytes(BytesN::from_array(env, &g1_bytes_from_ark(proof.a))),
        b: G2Affine::from_bytes(BytesN::from_array(env, &g2_bytes_from_ark(proof.b))),
        c: G1Affine::from_bytes(BytesN::from_array(env, &g1_bytes_from_ark(proof.c))),
    }
}

fn serialize_proof(env: &Env, proof: &Groth16Proof) -> Bytes {
    let mut data = Bytes::new(env);
    data.append(&Bytes::from(proof.a.to_bytes()));
    data.append(&Bytes::from(proof.b.to_bytes()));
    data.append(&Bytes::from(proof.c.to_bytes()));
    data
}

fn build_test(env: &Env) -> (VerificationKeyBytes, Groth16Proof, Vec<Bn254Fr>, [ArkFr; 9]) {
    let mut rng = seeded_rng();
    let inputs = [ArkFr::from(33u64); 9];
    let circuit = NineInputCircuit { inputs };
    let params =
        Groth16::<Bn254>::generate_random_parameters_with_reduction(circuit.clone(), &mut rng)
            .expect("params failed to generate");
    let proof = Groth16::<Bn254>::create_random_proof_with_reduction(circuit, &params, &mut rng)
        .expect("proof failed");

    let mut public_inputs: Vec<Bn254Fr> = Vec::new(env);
    for value in inputs {
        public_inputs.push_back(fr_from_ark(env, value));
    }

    let vk_bytes_ext = vk_bytes_from_ark(env, &params.vk);
    let vk_bytes = VerificationKeyBytes {
        alpha: vk_bytes_ext.alpha,
        beta: vk_bytes_ext.beta,
        gamma: vk_bytes_ext.gamma,
        delta: vk_bytes_ext.delta,
        ic: vk_bytes_ext.ic,
    };

    (
        vk_bytes,
        groth16_proof_from_ark(env, &proof),
        public_inputs,
        inputs,
    )
}

/// Create a test environment that disables snapshot writing under Miri.
/// Miri's isolation mode blocks filesystem operations, which the Soroban SDK
/// uses for test snapshots.
fn test_env() -> Env {
    #[cfg(miri)]
    {
        use soroban_sdk::testutils::EnvTestConfig;
        Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        })
    }
    #[cfg(not(miri))]
    {
        Env::default()
    }
}

#[test]
fn verifies_valid_proof() {
    let env = test_env();
    let (vk_bytes, proof, public_inputs, _) = build_test(&env);
    let vk = verification_key_from_bytes(&env, &vk_bytes);

    let result = CircomGroth16Verifier::verify_with_vk(&env, &vk, proof, public_inputs);

    assert_eq!(result, Ok(true));
}

#[test]
fn rejects_wrong_public_input_length() {
    let env = test_env();
    let (vk_bytes, proof, _public_inputs, inputs) = build_test(&env);
    let vk = verification_key_from_bytes(&env, &vk_bytes);

    // Provide too few public inputs (length 5 instead of 11)
    let mut short_inputs: Vec<Bn254Fr> = Vec::new(&env);
    for value in inputs.iter().take(5) {
        short_inputs.push_back(fr_from_ark(&env, *value));
    }

    let result = CircomGroth16Verifier::verify_with_vk(&env, &vk, proof, short_inputs);
    assert!(matches!(result, Err(Groth16Error::MalformedPublicInputs)));
}

#[test]
fn groth16_proof_parsing_checks_size() {
    let env = test_env();
    let (_vk_bytes, proof, _public_inputs, _) = build_test(&env);

    let encoded = serialize_proof(&env, &proof);
    assert_eq!(encoded.len(), PROOF_SIZE);
    assert!(Groth16Proof::try_from(encoded.clone()).is_ok());

    let truncated = encoded.slice(0..(PROOF_SIZE - 1));
    assert!(matches!(
        Groth16Proof::try_from(truncated),
        Err(Groth16Error::MalformedProof)
    ));
}

#[test]
fn policy_vk_matches_pool_boundary_shape() {
    let vk: Value =
        serde_json::from_str(include_str!("../../../circuits/keys/policy_tx_2_2_vk.json"))
            .unwrap_or_else(|err| panic!("policy transaction VK should parse: {err}"));

    let public_input_count = vk
        .get("nPublic")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("policy transaction VK should expose nPublic"));
    let ic = vk
        .get("IC")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("policy transaction VK should expose IC"));

    assert_eq!(public_input_count, 9);
    assert_eq!(ic.len(), 10);
}

/// Decode a decimal field element string into a Soroban BN254 scalar.
fn fr_from_decimal(env: &Env, decimal: &str) -> Bn254Fr {
    let value = decimal
        .parse::<num_bigint::BigUint>()
        .unwrap_or_else(|err| panic!("public input should be a decimal integer: {err}"));
    let be = value.to_bytes_be();
    let offset = 32usize
        .checked_sub(be.len())
        .unwrap_or_else(|| panic!("public input should fit in 32 bytes"));
    let mut buf = [0u8; 32];
    buf[offset..].copy_from_slice(&be);
    Bn254Fr::from_bytes(BytesN::from_array(env, &buf))
}

/// The committed fixture is a real proof over the `policy_tx_2_2` circuit,
/// generated with the proving key in `circuits/keys`. This is the end-to-end
/// check that a proof the client can actually produce verifies against the
/// verification key embedded in this contract.
#[test]
fn verifies_the_real_policy_transaction_proof() {
    let env = test_env();

    let proof_json: Value = serde_json::from_str(include_str!(
        "../../../circuits/fixtures/policy_tx_2_2_proof.json"
    ))
    .unwrap_or_else(|err| panic!("proof fixture should parse: {err}"));
    let public_json: Value = serde_json::from_str(include_str!(
        "../../../circuits/fixtures/policy_tx_2_2_public_inputs.json"
    ))
    .unwrap_or_else(|err| panic!("public input fixture should parse: {err}"));

    let proof_hex = proof_json
        .get("proofBytes")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("proof fixture should carry proofBytes"));
    let proof_bytes = hex::decode(proof_hex.trim_start_matches("0x"))
        .unwrap_or_else(|err| panic!("proofBytes should be hex: {err}"));
    let proof = Groth16Proof::try_from(Bytes::from_slice(&env, &proof_bytes))
        .unwrap_or_else(|_| panic!("proof fixture should decode into a Groth16 proof"));

    let values = public_json
        .get("values")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("public input fixture should carry values"));
    assert_eq!(values.len(), 9);

    let mut public_inputs: Vec<Bn254Fr> = Vec::new(&env);
    for value in values {
        let decimal = value
            .get("decimal")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("public input should carry a decimal"));
        public_inputs.push_back(fr_from_decimal(&env, decimal));
    }

    let id = env.register(CircomGroth16Verifier, ());
    let client = CircomGroth16VerifierClient::new(&env, &id);

    assert!(client.verify(&proof, &public_inputs));
}

/// Flipping one public input must break the pairing check.
#[test]
fn rejects_the_real_proof_under_a_tampered_public_input() {
    let env = test_env();

    let proof_json: Value = serde_json::from_str(include_str!(
        "../../../circuits/fixtures/policy_tx_2_2_proof.json"
    ))
    .unwrap_or_else(|err| panic!("proof fixture should parse: {err}"));
    let public_json: Value = serde_json::from_str(include_str!(
        "../../../circuits/fixtures/policy_tx_2_2_public_inputs.json"
    ))
    .unwrap_or_else(|err| panic!("public input fixture should parse: {err}"));

    let proof_hex = proof_json
        .get("proofBytes")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("proof fixture should carry proofBytes"));
    let proof_bytes = hex::decode(proof_hex.trim_start_matches("0x"))
        .unwrap_or_else(|err| panic!("proofBytes should be hex: {err}"));
    let proof = Groth16Proof::try_from(Bytes::from_slice(&env, &proof_bytes))
        .unwrap_or_else(|_| panic!("proof fixture should decode into a Groth16 proof"));

    let values = public_json
        .get("values")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("public input fixture should carry values"));

    let mut public_inputs: Vec<Bn254Fr> = Vec::new(&env);
    for (index, value) in values.iter().enumerate() {
        let decimal = value
            .get("decimal")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("public input should carry a decimal"));
        // Tamper with the pool root, the first public input.
        let decimal = if index == 0 { "1" } else { decimal };
        public_inputs.push_back(fr_from_decimal(&env, decimal));
    }

    let id = env.register(CircomGroth16Verifier, ());
    let client = CircomGroth16VerifierClient::new(&env, &id);

    assert!(client.try_verify(&proof, &public_inputs).is_err());
}
