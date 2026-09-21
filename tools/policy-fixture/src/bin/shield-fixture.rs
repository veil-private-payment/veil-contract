//! Produce the values a shield needs, bound to a live pool.
//!
//! The pool derives `extDataHash` from the XDR encoding of its own `ExtData`,
//! and it only accepts a root it has held, so both come from the deployed
//! contract. Both input notes are empty, which is what lets value enter a pool
//! that has never seen this wallet before.
//!
//! Usage: cargo run -p policy-fixture --bin shield-fixture -- <ext_data_hash_hex> <amount> [root_hex]
use anyhow::{Context, Result, ensure};
use policy_fixture::{generate_first_shield, generate_shield_against_root};
use serde_json::json;

fn hex32(arg: &str, what: &str) -> Result<[u8; 32]> {
    let raw = hex::decode(arg.trim().trim_start_matches("0x"))
        .with_context(|| format!("{what} must be hex"))?;
    ensure!(raw.len() == 32, "{what} must be 32 bytes");
    let mut out = [0u8; 32];
    out.copy_from_slice(&raw);
    Ok(out)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let ext_data_hash = hex32(
        &args.next().context("expected the ext data hash")?,
        "ext data hash",
    )?;
    let amount: i64 = args
        .next()
        .context("expected the amount to shield")?
        .trim()
        .parse()
        .context("amount must be an integer")?;

    let fixture = match args.next() {
        Some(root) => generate_shield_against_root(ext_data_hash, amount, hex32(&root, "root")?)?,
        None => generate_first_shield(ext_data_hash, amount)?,
    };

    let public = fixture.public_inputs_be();
    ensure!(public.len() == 9, "expected 9 public inputs");
    let dec = |b: &[u8; 32]| num_bigint::BigUint::from_bytes_be(b).to_string();

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "extDataHash": hex::encode(ext_data_hash),
            "extAmount": amount,
            "proofHex": hex::encode(fixture.proof_bytes()),
            "membershipLeaves": fixture.membership_leaves().iter().map(dec).collect::<Vec<_>>(),
            "root": dec(&public[0]),
            "publicAmount": dec(&public[1]),
            "inputNullifiers": [dec(&public[3]), dec(&public[4])],
            "outputCommitments": [dec(&public[5]), dec(&public[6])],
            "aspMembershipRoot": dec(&public[7]),
        }))?
    );
    Ok(())
}
