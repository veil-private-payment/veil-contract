//! Regenerate the `circuits/keys/policy_tx_2_2_*` key artifacts from the canonical
//! snarkjs `policy_final.zkey` (the ceremony the client/FE proves with), so the
//! embedded verifier VK, the Soroban VK, and the Arkworks proving key all match
//! the proofs users actually generate.
//!
//! Reads the snarkjs zkey into an Arkworks proving key (`ark-circom`), then
//! writes:
//!   - `policy_tx_2_2_proving_key.bin`  (Arkworks pk, used by `policy-fixture`)
//!   - `policy_tx_2_2_vk.json`          (snarkjs VK, embedded by verifier build.rs)
//!   - `policy_tx_2_2_vk_soroban.bin`   (Soroban-encoded VK)
//!   - `policy_tx_2_2_vk_const.rs`      (Rust VK constants)
//!
//! Usage: cargo run -p policy-fixture --bin regen-from-zkey [path/to/final.zkey]
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ark_bn254::Bn254;
use ark_circom::read_zkey;
use ark_groth16::ProvingKey;
use circuit_keys::{
    write_proving_key_bin, write_vk_rust_const, write_vk_snarkjs_json, write_vk_soroban_bin,
};

fn main() -> Result<()> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let zkey_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("circuits/keys/policy_tx_2_2_proving_key.zkey"));
    let keys = repo_root.join("circuits/keys");

    let mut reader = BufReader::new(
        File::open(&zkey_path)
            .with_context(|| format!("failed to open zkey {}", zkey_path.display()))?,
    );
    let (pk, _matrices): (ProvingKey<Bn254>, _) =
        read_zkey(&mut reader).context("failed to parse snarkjs zkey")?;

    write_proving_key_bin(&pk, &keys.join("policy_tx_2_2_proving_key.bin"))?;
    write_vk_snarkjs_json(&pk.vk, &keys.join("policy_tx_2_2_vk.json"))?;
    write_vk_soroban_bin(&pk.vk, &keys.join("policy_tx_2_2_vk_soroban.bin"))?;
    write_vk_rust_const(&pk.vk, &keys.join("policy_tx_2_2_vk_const.rs"))?;

    println!(
        "Regenerated circuits/keys/policy_tx_2_2_* from {}",
        zkey_path.display()
    );
    Ok(())
}
