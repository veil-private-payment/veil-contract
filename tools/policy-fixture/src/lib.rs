use anyhow::{Context, Result, anyhow, ensure};
use ark_bn254::{Bn254, Fr, G1Affine, G2Affine};
use ark_circom::{CircomBuilder, CircomConfig, CircomReduction};
use ark_ff::{BigInteger, PrimeField, Zero};
use ark_groth16::{Groth16, Proof, ProvingKey};
use ark_serialize::CanonicalDeserialize;
use ark_snark::SNARK;
use ark_std::rand::{SeedableRng, rngs::StdRng};
use circuit_keys::{g1_to_soroban_bytes, g2_to_soroban_bytes};
use num_bigint::{BigInt, BigUint, Sign};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::BufReader,
    ops::AddAssign,
    path::{Path, PathBuf},
};
use zkhash::{
    fields::bn256::FpBN256 as Scalar,
    poseidon2::{
        poseidon2::Poseidon2,
        poseidon2_instance_bn256::{
            POSEIDON2_BN256_PARAMS_2, POSEIDON2_BN256_PARAMS_3, POSEIDON2_BN256_PARAMS_4,
        },
    },
};

const CIRCUIT_NAME: &str = "policy_tx_2_2";
const LEVELS: usize = 10;
const PROOF_RNG_SEED: u64 = 20_260_612;

/// Value the contracts use for an unfilled Merkle leaf.
///
/// The pool and the ASP membership contract seed empty subtrees with this
/// constant rather than with zero, so a fixture that pads with zero computes a
/// different root than the live tree. See `soroban_utils::get_zeroes`.
const EMPTY_LEAF_DECIMAL: &str =
    "16820622405745174042249830601237189755928192602553897283642901160942722677198";

fn empty_leaf() -> Scalar {
    let value = EMPTY_LEAF_DECIMAL
        .parse::<BigUint>()
        .unwrap_or_else(|err| panic!("empty leaf constant should parse: {err}"));
    Scalar::from(value)
}

const PUBLIC_INPUT_NAMES: [&str; 9] = [
    "root",
    "public_amount",
    "ext_data_hash",
    "input_nullifier0",
    "input_nullifier1",
    "output_commitment0",
    "output_commitment1",
    "asp_membership_root0",
    "asp_membership_root1",
];

#[derive(Clone, Copy)]
struct InputNote {
    leaf_index: usize,
    private_key: Scalar,
    blinding: Scalar,
    amount: Scalar,
}

#[derive(Clone, Copy)]
struct OutputNote {
    public_key: Scalar,
    blinding: Scalar,
    amount: Scalar,
}

struct PolicyInputs {
    values: Vec<(String, Vec<BigInt>)>,
    witness_json: Value,
    input_json: Value,
    input_commitments: Vec<[u8; 32]>,
    membership_leaves: Vec<[u8; 32]>,
}

/// A generated `policy_tx_2_2` fixture: the Groth16 proof plus its public inputs.
///
/// This is the in-memory result of [`generate`], reusable by callers that want
/// to submit the proof somewhere (e.g. an on-chain verifier) without going
/// through the fixture JSON files.
pub struct Fixture {
    /// The Groth16 proof over BN254.
    pub proof: Proof<Bn254>,
    /// The 9 public inputs, in circuit order (see [`PUBLIC_INPUT_NAMES`]).
    pub public_inputs: Vec<Fr>,
    /// Circuit-level inputs used to render the witness/input JSON files.
    policy_inputs: PolicyInputs,
}

impl Fixture {
    /// Commitments of the input notes, in pool leaf order.
    ///
    /// Depositing these in order rebuilds the pool Merkle tree the proof was
    /// generated against.
    pub fn input_commitments(&self) -> &[[u8; 32]] {
        &self.policy_inputs.input_commitments
    }

    /// ASP membership leaves, in enrollment order.
    ///
    /// Inserting these into the membership contract in order reproduces the
    /// membership root the proof was generated against.
    pub fn membership_leaves(&self) -> &[[u8; 32]] {
        &self.policy_inputs.membership_leaves
    }

    /// Public inputs in circuit order, big-endian encoded.
    pub fn public_inputs_be(&self) -> Vec<[u8; 32]> {
        self.public_inputs.iter().map(fr_to_be_bytes).collect()
    }

    /// The proof in the byte layout the Soroban verifier expects.
    pub fn proof_bytes(&self) -> Vec<u8> {
        proof_bytes(&self.proof.a, &self.proof.b, &self.proof.c)
    }
}

/// Read a big-endian 32-byte value as a field element.
pub fn scalar_from_be_bytes(bytes: &[u8; 32]) -> Scalar {
    let value = BigUint::from_bytes_be(bytes);
    Scalar::from(value)
}

/// Big-endian 32-byte encoding of a field element.
pub fn fr_to_be_bytes(value: &Fr) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut buf = [0u8; 32];
    let offset = buf.len().saturating_sub(bytes.len());
    buf[offset..].copy_from_slice(&bytes);
    buf
}

/// Generate the deterministic `policy_tx_2_2` proof in memory.
///
/// Loads the compiled circuit artifacts + proving key, builds the witness, and
/// produces a Groth16 proof. Does not touch the fixture files; use
/// [`write_fixture_files`] for that.
pub fn generate() -> Result<Fixture> {
    generate_with_ext_data_hash([0u8; 32])
}

/// Same as [`generate`], but binds the proof to a caller-supplied
/// `extDataHash`.
///
/// The pool derives that hash from the XDR encoding of its own `ExtData`
/// struct, which only a Soroban environment can produce. Contract tests
/// compute it there and pass it in here so the resulting proof satisfies the
/// pool's external data check.
pub fn generate_with_ext_data_hash(ext_data_hash: [u8; 32]) -> Result<Fixture> {
    let ext_data_hash = scalar_from_be_bytes(&ext_data_hash);
    let repo_root = repo_root()?;
    let artifact_dir = repo_root.join("target/circuits-artifacts/manual");
    let wasm_path = artifact_dir.join("policy_tx_2_2_js/policy_tx_2_2.wasm");
    let r1cs_path = artifact_dir.join("policy_tx_2_2.r1cs");
    let proving_key_path = repo_root.join("circuits/keys/policy_tx_2_2_proving_key.bin");

    ensure!(
        wasm_path.exists() && r1cs_path.exists(),
        "compiled circuit artifacts are missing; run `make compile-policy-circuit` first"
    );

    let policy_inputs = build_policy_inputs(ext_data_hash)?;
    let proof_result = prove_and_verify(&wasm_path, &r1cs_path, &proving_key_path, &policy_inputs)?;

    Ok(Fixture {
        proof: proof_result.proof,
        public_inputs: proof_result.public_inputs,
        policy_inputs,
    })
}

/// Generate the fixture and write the four `circuits/fixtures/policy_tx_2_2_*.json` files.
pub fn write_fixture_files() -> Result<()> {
    let repo_root = repo_root()?;
    let fixture_dir = repo_root.join("circuits/fixtures");

    let fixture = generate()?;

    fs::create_dir_all(&fixture_dir)
        .with_context(|| format!("failed to create {}", fixture_dir.display()))?;

    write_json_pretty(
        &fixture_dir.join("policy_tx_2_2_witness_inputs.json"),
        &fixture.policy_inputs.witness_json,
    )?;
    write_json_pretty(
        &fixture_dir.join("policy_tx_2_2_input.json"),
        &fixture.policy_inputs.input_json,
    )?;
    write_json_pretty(
        &fixture_dir.join("policy_tx_2_2_public_inputs.json"),
        &public_inputs_json(&fixture.public_inputs)?,
    )?;
    write_json_pretty(
        &fixture_dir.join("policy_tx_2_2_proof.json"),
        &proof_json(&fixture.proof),
    )?;

    println!(
        "Generated deterministic {CIRCUIT_NAME} fixture in {}",
        fixture_dir.display()
    );
    Ok(())
}

/// Encode the proof points into Soroban verifier byte layout: A (G1, 64),
/// B (G2, 128), C (G1, 64). This matches what the on-chain `verify` expects.
pub fn proof_soroban_parts(proof: &Proof<Bn254>) -> ([u8; 64], [u8; 128], [u8; 64]) {
    (
        g1_to_soroban_bytes(&proof.a),
        g2_to_soroban_bytes(&proof.b),
        g1_to_soroban_bytes(&proof.c),
    )
}

/// Encode each public input as a 32-byte big-endian field element, matching the
/// `Vec<Bn254Fr>` (u256) the on-chain verifier consumes.
pub fn public_input_be_bytes(public_inputs: &[Fr]) -> Vec<[u8; 32]> {
    public_inputs
        .iter()
        .map(|value| {
            let mut out = [0u8; 32];
            let bytes = value.into_bigint().to_bytes_be();
            let start = out.len().saturating_sub(bytes.len());
            out[start..].copy_from_slice(&bytes);
            out
        })
        .collect()
}

struct ProofResult {
    proof: Proof<Bn254>,
    public_inputs: Vec<Fr>,
}

fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tools_dir = manifest_dir
        .parent()
        .ok_or_else(|| anyhow!("policy-fixture manifest has no parent"))?;
    let repo_root = tools_dir
        .parent()
        .ok_or_else(|| anyhow!("tools directory has no parent"))?;
    Ok(repo_root.to_path_buf())
}

fn prove_and_verify(
    wasm_path: &Path,
    r1cs_path: &Path,
    proving_key_path: &Path,
    inputs: &PolicyInputs,
) -> Result<ProofResult> {
    let file = File::open(proving_key_path)
        .with_context(|| format!("failed to open {}", proving_key_path.display()))?;
    let mut reader = BufReader::new(file);
    let pk = ProvingKey::<Bn254>::deserialize_compressed(&mut reader)
        .map_err(|err| anyhow!("failed to deserialize proving key: {err}"))?;
    let pvk = Groth16::<Bn254, CircomReduction>::process_vk(&pk.vk)
        .map_err(|err| anyhow!("failed to process VK: {err}"))?;

    let cfg = CircomConfig::<Fr>::new(wasm_path, r1cs_path)
        .map_err(|err| anyhow!("failed to load circom config: {err}"))?;
    let mut builder = CircomBuilder::new(cfg);

    for (signal, values) in &inputs.values {
        for value in values {
            builder.push_input(signal, value.clone());
        }
    }

    let circuit = builder
        .build()
        .map_err(|err| anyhow!("failed to build circom witness: {err}"))?;
    let public_inputs = circuit
        .get_public_inputs()
        .ok_or_else(|| anyhow!("circom circuit did not expose public inputs"))?;
    ensure!(
        public_inputs.len() == PUBLIC_INPUT_NAMES.len(),
        "expected {} public inputs, got {}",
        PUBLIC_INPUT_NAMES.len(),
        public_inputs.len()
    );

    let mut rng = StdRng::seed_from_u64(PROOF_RNG_SEED);
    let proof = Groth16::<Bn254, CircomReduction>::prove(&pk, circuit, &mut rng)
        .map_err(|err| anyhow!("failed to generate Groth16 proof: {err}"))?;
    let verified =
        Groth16::<Bn254, CircomReduction>::verify_with_processed_vk(&pvk, &public_inputs, &proof)
            .map_err(|err| anyhow!("failed to verify generated proof: {err}"))?;
    ensure!(verified, "generated proof did not verify");

    Ok(ProofResult {
        proof,
        public_inputs,
    })
}

fn build_policy_inputs(ext_data_hash: Scalar) -> Result<PolicyInputs> {
    let inputs = [
        InputNote {
            leaf_index: 0,
            private_key: Scalar::from(101u64),
            blinding: Scalar::from(201u64),
            amount: Scalar::zero(),
        },
        InputNote {
            // The pool inserts commitments in two-leaf batches, so a deposited
            // commitment always lands on an even leaf. Keeping the fixture on
            // leaf 2 lets a pool built by two deposits reproduce this root.
            leaf_index: 2,
            private_key: Scalar::from(102u64),
            blinding: Scalar::from(211u64),
            amount: Scalar::from(13u64),
        },
    ];
    let outputs = [
        OutputNote {
            public_key: Scalar::from(501u64),
            blinding: Scalar::from(601u64),
            amount: Scalar::from(13u64),
        },
        OutputNote {
            public_key: Scalar::from(502u64),
            blinding: Scalar::from(602u64),
            amount: Scalar::zero(),
        },
    ];

    let leaf_count = leaf_count(LEVELS)?;
    let mut tx_leaves = vec![empty_leaf(); leaf_count];
    let mut public_keys = Vec::with_capacity(inputs.len());
    let mut input_commitments = Vec::with_capacity(inputs.len());

    for note in inputs {
        ensure!(
            note.leaf_index < tx_leaves.len(),
            "input leaf index {} is outside tree",
            note.leaf_index
        );
        let public_key = derive_public_key(note.private_key);
        let cm = commitment(note.amount, public_key, note.blinding);
        tx_leaves[note.leaf_index] = cm;
        // `deposit` inserts a two-leaf batch of (commitment, zero), so the leaf
        // paired with a deposited commitment holds a literal zero rather than
        // the empty-leaf constant used for subtrees that were never touched.
        if let Some(pair_slot) = tx_leaves.get_mut(note.leaf_index.saturating_add(1)) {
            *pair_slot = Scalar::zero();
        }
        public_keys.push(public_key);
        input_commitments.push(cm);
    }

    let root = merkle_root(tx_leaves.clone())?;
    let mut path_indices = Vec::with_capacity(inputs.len());
    let mut path_elements_flat = Vec::with_capacity(inputs.len().saturating_mul(LEVELS));
    let mut input_path_elements: Vec<Vec<Scalar>> = Vec::with_capacity(inputs.len());
    let mut nullifiers = Vec::with_capacity(inputs.len());

    for (index, note) in inputs.iter().enumerate() {
        let (path_elements, path_index, levels) = merkle_proof(&tx_leaves, note.leaf_index)?;
        ensure!(
            levels == LEVELS,
            "unexpected merkle proof depth for input {index}: {levels}"
        );
        let path_index_scalar = Scalar::from(path_index);
        let sig = sign(
            note.private_key,
            input_commitments[index],
            path_index_scalar,
        );
        nullifiers.push(nullifier(input_commitments[index], path_index_scalar, sig));
        path_indices.push(path_index_scalar);
        input_path_elements.push(path_elements.clone());
        path_elements_flat.extend(path_elements);
    }

    let mut membership_leaves = vec![empty_leaf(); leaf_count];
    let membership_blinding = Scalar::zero();
    // The ASP tree is enrolled independently of the pool tree: the membership
    // contract appends one leaf per enrollment, so slot order is what a live
    // allowlist reproduces.
    for (leaf_slot, public_key) in public_keys.iter().enumerate() {
        membership_leaves[leaf_slot] =
            poseidon2_hash2(*public_key, membership_blinding, Some(Scalar::from(1u64)));
    }
    let membership_root = merkle_root(membership_leaves.clone())?;

    let mut membership_path_elements: Vec<Vec<Scalar>> = Vec::with_capacity(inputs.len());
    let mut membership_path_indices: Vec<u64> = Vec::with_capacity(inputs.len());

    let mut records = Vec::new();
    push_scalar(&mut records, "root", root);
    push_scalar(&mut records, "publicAmount", Scalar::zero());
    push_scalar(&mut records, "extDataHash", ext_data_hash);
    push_scalars(&mut records, "inputNullifier", &nullifiers);
    push_scalars(
        &mut records,
        "outputCommitment",
        &outputs
            .iter()
            .map(|note| commitment(note.amount, note.public_key, note.blinding))
            .collect::<Vec<_>>(),
    );
    push_scalars(
        &mut records,
        "membershipRoots",
        &vec![membership_root; inputs.len()],
    );

    for (index, public_key) in public_keys.iter().enumerate() {
        let leaf = poseidon2_hash2(*public_key, membership_blinding, Some(Scalar::from(1u64)));
        let (membership_path, membership_path_index, levels) =
            merkle_proof(&membership_leaves, index)?;
        ensure!(
            levels == LEVELS,
            "unexpected membership proof depth for input {index}: {levels}"
        );
        membership_path_elements.push(membership_path.clone());
        membership_path_indices.push(membership_path_index);

        let prefix = format!("membershipProofs[{index}][0]");
        push_scalar(&mut records, &format!("{prefix}.leaf"), leaf);
        push_scalar(
            &mut records,
            &format!("{prefix}.blinding"),
            membership_blinding,
        );
        push_scalar(
            &mut records,
            &format!("{prefix}.pathIndices"),
            Scalar::from(membership_path_index),
        );
        push_scalars(
            &mut records,
            &format!("{prefix}.pathElements"),
            &membership_path,
        );
    }

    push_scalars(
        &mut records,
        "inAmount",
        &inputs.iter().map(|note| note.amount).collect::<Vec<_>>(),
    );
    push_scalars(
        &mut records,
        "inPrivateKey",
        &inputs
            .iter()
            .map(|note| note.private_key)
            .collect::<Vec<_>>(),
    );
    push_scalars(
        &mut records,
        "inBlinding",
        &inputs.iter().map(|note| note.blinding).collect::<Vec<_>>(),
    );
    push_scalars(&mut records, "inPathIndices", &path_indices);
    push_scalars(&mut records, "inPathElements", &path_elements_flat);
    push_scalars(
        &mut records,
        "outAmount",
        &outputs.iter().map(|note| note.amount).collect::<Vec<_>>(),
    );
    push_scalars(
        &mut records,
        "outPubkey",
        &outputs
            .iter()
            .map(|note| note.public_key)
            .collect::<Vec<_>>(),
    );
    push_scalars(
        &mut records,
        "outBlinding",
        &outputs.iter().map(|note| note.blinding).collect::<Vec<_>>(),
    );

    // Nested, circom/snarkjs-compatible input.json (signal names match the
    // `policy_tx_2_2` main component, buses as nested objects).
    let dec = |value: Scalar| scalar_to_bigint(value).to_string();
    let zero_dec = "0".to_string();
    let membership_proofs_json = (0..inputs.len())
        .map(|i| {
            let leaf = poseidon2_hash2(public_keys[i], membership_blinding, Some(Scalar::from(1u64)));
            json!([{
                "leaf": dec(leaf),
                "blinding": dec(membership_blinding),
                "pathElements": membership_path_elements[i].iter().map(|v| dec(*v)).collect::<Vec<_>>(),
                "pathIndices": membership_path_indices[i].to_string(),
            }])
        })
        .collect::<Vec<_>>();
    let input_json = json!({
        "root": dec(root),
        "publicAmount": zero_dec.clone(),
        "extDataHash": dec(ext_data_hash),
        "inputNullifier": nullifiers.iter().map(|v| dec(*v)).collect::<Vec<_>>(),
        "outputCommitment": outputs.iter()
            .map(|note| dec(commitment(note.amount, note.public_key, note.blinding)))
            .collect::<Vec<_>>(),
        "membershipRoots": inputs.iter().map(|_| vec![dec(membership_root)]).collect::<Vec<_>>(),
        "membershipProofs": membership_proofs_json,
        "inAmount": inputs.iter().map(|note| dec(note.amount)).collect::<Vec<_>>(),
        "inPrivateKey": inputs.iter().map(|note| dec(note.private_key)).collect::<Vec<_>>(),
        "inBlinding": inputs.iter().map(|note| dec(note.blinding)).collect::<Vec<_>>(),
        "inPathIndices": path_indices.iter().map(|v| dec(*v)).collect::<Vec<_>>(),
        "inPathElements": input_path_elements.iter()
            .map(|pe| pe.iter().map(|v| dec(*v)).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        "outAmount": outputs.iter().map(|note| dec(note.amount)).collect::<Vec<_>>(),
        "outPubkey": outputs.iter().map(|note| dec(note.public_key)).collect::<Vec<_>>(),
        "outBlinding": outputs.iter().map(|note| dec(note.blinding)).collect::<Vec<_>>(),
    });

    let witness_json = json!({
        "circuit": CIRCUIT_NAME,
        "case": "one-real-input-one-real-output-with-dummy-slots",
        "levels": LEVELS,
        "proofRngSeed": PROOF_RNG_SEED,
        "root": scalar_json(root),
        "publicAmount": scalar_json(Scalar::zero()),
        "extDataHash": scalar_json(ext_data_hash),
        "inputNullifiers": nullifiers.iter().map(|v| scalar_json(*v)).collect::<Vec<_>>(),
        "outputCommitments": outputs.iter()
            .map(|note| scalar_json(commitment(note.amount, note.public_key, note.blinding)))
            .collect::<Vec<_>>(),
        "membershipRoot": scalar_json(membership_root),
        "inputs": inputs.iter().enumerate().map(|(index, note)| {
            json!({
                "slot": index,
                "leafIndex": note.leaf_index,
                "privateKey": scalar_json(note.private_key),
                "publicKey": scalar_json(public_keys[index]),
                "blinding": scalar_json(note.blinding),
                "amount": scalar_json(note.amount),
            })
        }).collect::<Vec<_>>(),
        "outputs": outputs.iter().enumerate().map(|(index, note)| {
            json!({
                "slot": index,
                "publicKey": scalar_json(note.public_key),
                "blinding": scalar_json(note.blinding),
                "amount": scalar_json(note.amount),
            })
        }).collect::<Vec<_>>(),
    });

    let input_commitment_bytes = input_commitments
        .iter()
        .map(|value| scalar_to_be_bytes(*value))
        .collect::<Vec<_>>();
    let membership_leaf_bytes = membership_leaves
        .iter()
        .take(inputs.len())
        .map(|value| scalar_to_be_bytes(*value))
        .collect::<Vec<_>>();

    Ok(PolicyInputs {
        values: records,
        witness_json,
        input_json,
        input_commitments: input_commitment_bytes,
        membership_leaves: membership_leaf_bytes,
    })
}

fn push_scalar(records: &mut Vec<(String, Vec<BigInt>)>, signal: &str, value: Scalar) {
    push_bigint(records, signal, scalar_to_bigint(value));
}

fn push_scalars(records: &mut Vec<(String, Vec<BigInt>)>, signal: &str, values: &[Scalar]) {
    push_bigints(
        records,
        signal,
        &values
            .iter()
            .map(|value| scalar_to_bigint(*value))
            .collect::<Vec<_>>(),
    );
}

fn push_bigint(records: &mut Vec<(String, Vec<BigInt>)>, signal: &str, value: BigInt) {
    records.push((signal.to_string(), vec![value]));
}

fn push_bigints(records: &mut Vec<(String, Vec<BigInt>)>, signal: &str, values: &[BigInt]) {
    records.push((signal.to_string(), values.to_vec()));
}

fn derive_public_key(private_key: Scalar) -> Scalar {
    poseidon2_hash2(private_key, Scalar::zero(), Some(Scalar::from(3u64)))
}

fn sign(private_key: Scalar, commitment: Scalar, merkle_path: Scalar) -> Scalar {
    poseidon2_hash3(
        private_key,
        commitment,
        merkle_path,
        Some(Scalar::from(4u64)),
    )
}

fn commitment(amount: Scalar, public_key: Scalar, blinding: Scalar) -> Scalar {
    poseidon2_hash3(amount, public_key, blinding, Some(Scalar::from(1u64)))
}

fn nullifier(commitment: Scalar, path_indices: Scalar, signature: Scalar) -> Scalar {
    poseidon2_hash3(
        commitment,
        path_indices,
        signature,
        Some(Scalar::from(2u64)),
    )
}

fn poseidon2_compression(left: Scalar, right: Scalar) -> Scalar {
    let h = Poseidon2::new(&POSEIDON2_BN256_PARAMS_2);
    let mut perm = h.permutation(&[left, right]);
    perm[0].add_assign(&left);
    perm[0]
}

fn poseidon2_hash2(a: Scalar, b: Scalar, domain_separator: Option<Scalar>) -> Scalar {
    let h = Poseidon2::new(&POSEIDON2_BN256_PARAMS_3);
    let perm = h.permutation(&[a, b, domain_separator.unwrap_or_else(Scalar::zero)]);
    perm[0]
}

fn poseidon2_hash3(a: Scalar, b: Scalar, c: Scalar, domain_separator: Option<Scalar>) -> Scalar {
    let h = Poseidon2::new(&POSEIDON2_BN256_PARAMS_4);
    let perm = h.permutation(&[a, b, c, domain_separator.unwrap_or_else(Scalar::zero)]);
    perm[0]
}

fn leaf_count(levels: usize) -> Result<usize> {
    let shift = u32::try_from(levels).context("levels does not fit into u32")?;
    1usize
        .checked_shl(shift)
        .ok_or_else(|| anyhow!("levels are too large for usize"))
}

fn merkle_root(mut leaves: Vec<Scalar>) -> Result<Scalar> {
    ensure!(!leaves.is_empty(), "leaves cannot be empty");
    ensure!(
        leaves.len().is_power_of_two(),
        "leaves length must be a power of two"
    );

    while leaves.len() > 1 {
        let mut next = Vec::with_capacity(leaves.len() / 2);
        for pair in leaves.chunks_exact(2) {
            next.push(poseidon2_compression(pair[0], pair[1]));
        }
        leaves = next;
    }

    Ok(leaves[0])
}

fn merkle_proof(leaves: &[Scalar], mut index: usize) -> Result<(Vec<Scalar>, u64, usize)> {
    ensure!(
        !leaves.is_empty() && leaves.len().is_power_of_two(),
        "invalid leaves length"
    );
    ensure!(index < leaves.len(), "index outside merkle tree");

    let mut level_nodes = leaves.to_vec();
    let levels = usize::try_from(level_nodes.len().ilog2()).context("levels overflow")?;
    let mut path_elements = Vec::with_capacity(levels);
    let mut path_indices = 0u64;

    for level in 0..levels {
        let sibling_index = if index.is_multiple_of(2) {
            index
                .checked_add(1)
                .ok_or_else(|| anyhow!("sibling index overflow"))?
        } else {
            index
                .checked_sub(1)
                .ok_or_else(|| anyhow!("sibling index underflow"))?
        };
        path_elements.push(level_nodes[sibling_index]);

        if index % 2 == 1 {
            let shift = u32::try_from(level).context("path level overflow")?;
            path_indices |= 1u64
                .checked_shl(shift)
                .ok_or_else(|| anyhow!("path index overflow"))?;
        }

        let mut next = Vec::with_capacity(level_nodes.len() / 2);
        for pair in level_nodes.chunks_exact(2) {
            next.push(poseidon2_compression(pair[0], pair[1]));
        }
        level_nodes = next;
        index /= 2;
    }

    Ok((path_elements, path_indices, levels))
}

fn scalar_to_be_bytes(value: Scalar) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut buf = [0u8; 32];
    let offset = buf.len().saturating_sub(bytes.len());
    buf[offset..].copy_from_slice(&bytes);
    buf
}

fn scalar_to_bigint(value: Scalar) -> BigInt {
    let bytes = value.into_bigint().to_bytes_be();
    BigInt::from_bytes_be(Sign::Plus, &bytes)
}

fn scalar_json(value: Scalar) -> Value {
    json!({
        "decimal": scalar_to_bigint(value).to_string(),
        "hex": prime_field_hex(value),
    })
}

fn public_inputs_json(public_inputs: &[Fr]) -> Result<Value> {
    ensure!(
        public_inputs.len() == PUBLIC_INPUT_NAMES.len(),
        "public input count mismatch"
    );

    let values = PUBLIC_INPUT_NAMES
        .iter()
        .zip(public_inputs.iter())
        .map(|(name, value)| {
            json!({
                "name": name,
                "decimal": prime_field_decimal(*value),
                "hex": prime_field_hex(*value),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "circuit": CIRCUIT_NAME,
        "order": PUBLIC_INPUT_NAMES,
        "values": values,
    }))
}

fn proof_json(proof: &Proof<Bn254>) -> Value {
    let a = g1_to_soroban_bytes(&proof.a);
    let b = g2_to_soroban_bytes(&proof.b);
    let c = g1_to_soroban_bytes(&proof.c);
    let proof_bytes = proof_bytes(&proof.a, &proof.b, &proof.c);

    json!({
        "circuit": CIRCUIT_NAME,
        "encoding": "soroban-groth16-a-g1-b-g2-c-g1",
        "proofRngSeed": PROOF_RNG_SEED,
        "a": hex_prefixed(&a),
        "b": hex_prefixed(&b),
        "c": hex_prefixed(&c),
        "proofBytes": hex_prefixed(&proof_bytes),
    })
}

fn proof_bytes(a: &G1Affine, b: &G2Affine, c: &G1Affine) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(&g1_to_soroban_bytes(a));
    bytes.extend_from_slice(&g2_to_soroban_bytes(b));
    bytes.extend_from_slice(&g1_to_soroban_bytes(c));
    bytes
}

fn prime_field_decimal<F: PrimeField>(value: F) -> String {
    let bytes = value.into_bigint().to_bytes_be();
    BigInt::from_bytes_be(Sign::Plus, &bytes).to_string()
}

fn prime_field_hex<F: PrimeField>(value: F) -> String {
    let mut out = [0u8; 32];
    let bytes = value.into_bigint().to_bytes_be();
    let start = out.len().saturating_sub(bytes.len());
    out[start..].copy_from_slice(&bytes);
    hex_prefixed(&out)
}

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn write_json_pretty(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}
