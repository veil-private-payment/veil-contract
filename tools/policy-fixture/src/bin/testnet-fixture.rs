//! Produce the values a testnet demo needs, bound to a pool's own ext data hash.
//!
//! The pool derives `extDataHash` from the XDR encoding of its `ExtData`, so a
//! proof for a live pool must be generated against the value that pool reports
//! from `get_ext_data_hash`. Pass that hex string here and this writes every
//! value the deploy script feeds back to the contracts.
//!
//! An optional second argument is the external amount: negative withdraws that
//! many tokens from the pool, zero is a pure shielded transfer.
//!
//! Usage: cargo run -p policy-fixture --bin testnet-fixture -- <ext_data_hash_hex> [ext_amount]
use anyhow::{Context, Result, ensure};
use policy_fixture::generate_with_ext_data_hash_and_amount;
use serde_json::json;

fn main() -> Result<()> {
    let hex_arg = std::env::args()
        .nth(1)
        .context("expected the ext data hash as the first argument")?;
    let raw = hex::decode(hex_arg.trim().trim_start_matches("0x"))
        .context("ext data hash must be hex")?;
    ensure!(raw.len() == 32, "ext data hash must be 32 bytes");
    let mut ext_data_hash = [0u8; 32];
    ext_data_hash.copy_from_slice(&raw);

    let ext_amount: i64 = match std::env::args().nth(2) {
        Some(raw) => raw
            .trim()
            .parse()
            .context("external amount must be an integer")?,
        None => 0,
    };

    let fixture = generate_with_ext_data_hash_and_amount(ext_data_hash, ext_amount)?;
    let public = fixture.public_inputs_be();
    ensure!(public.len() == 9, "expected 9 public inputs");

    let to_dec = |bytes: &[u8; 32]| num_bigint::BigUint::from_bytes_be(bytes).to_string();

    let out = json!({
        "extDataHash": hex::encode(ext_data_hash),
        "extAmount": ext_amount,
        "proofHex": hex::encode(fixture.proof_bytes()),
        "inputCommitments": fixture.input_commitments().iter().map(to_dec).collect::<Vec<_>>(),
        "membershipLeaves": fixture.membership_leaves().iter().map(to_dec).collect::<Vec<_>>(),
        "root": to_dec(&public[0]),
        "publicAmount": to_dec(&public[1]),
        "inputNullifiers": [to_dec(&public[3]), to_dec(&public[4])],
        "outputCommitments": [to_dec(&public[5]), to_dec(&public[6])],
        "aspMembershipRoot": to_dec(&public[7]),
    });

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
