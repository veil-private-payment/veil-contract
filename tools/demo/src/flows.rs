//! The steps of a private payment, each reading the pool only through the API.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use ark_std::rand::SeedableRng;
use ark_std::rand::rngs::StdRng;
use num_bigint::BigUint;
use policy_fixture::{
    CreatedNote, Fixture, Membership, NoteScalar, SpentNote, TREE_LEVELS, Transaction,
    membership_leaf, note_commitment, note_nullifier, proof_soroban_parts, prove_transaction,
    scalar_from_be_bytes, scalar_to_be_bytes,
};
use x25519_dalek::StaticSecret;

use crate::api::{Api, Path};
use crate::chain::{Cli, Submitted};
use crate::ext_data::ExtData;
use crate::note_crypto::{self, Opened};
use crate::wallet::{Wallet, random_bytes, random_scalar};

pub struct Context {
    pub api: Api,
    pub cli: Cli,
    pub pool: String,
    pub faucet: String,
    pub wallets: PathBuf,
}

/// A note this wallet can spend.
#[derive(Clone, Debug)]
pub struct Owned {
    pub leaf_index: u64,
    pub amount: u128,
    pub blinding: NoteScalar,
    pub nullifier: String,
}

pub fn hex32(value: &str) -> Result<[u8; 32]> {
    hex::decode(value.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("{value} is not 32 bytes"))
}

fn scalar(value: &str) -> Result<NoteScalar> {
    Ok(scalar_from_be_bytes(&hex32(value)?))
}

pub fn scalar_hex(value: NoteScalar) -> String {
    format!("0x{}", hex::encode(scalar_to_be_bytes(value)))
}

fn decimal(bytes: &[u8; 32]) -> String {
    BigUint::from_bytes_be(bytes).to_string()
}

fn amount_bytes(amount: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&amount.to_be_bytes());
    out
}

fn path_scalars(path: &Path) -> Result<Vec<NoteScalar>> {
    path.path_elements
        .iter()
        .map(|element| scalar(element))
        .collect()
}

/// A note worth nothing, spent only to fill an input slot. Its blinding is
/// fresh, so its nullifier is new.
fn dummy_input(wallet: &Wallet) -> Result<SpentNote> {
    Ok(SpentNote {
        private_key: wallet.note_private_key,
        blinding: random_scalar()?,
        amount: 0,
        leaf_index: 0,
        path_elements: vec![NoteScalar::from(0u64); TREE_LEVELS],
    })
}

/// A new note for `public_key`, and its encryption to `encryption_key`.
fn new_note(
    public_key: NoteScalar,
    encryption_key: &[u8; 32],
    amount: u128,
) -> Result<(CreatedNote, Vec<u8>)> {
    let blinding = random_scalar()?;
    let encrypted = note_crypto::encrypt(
        encryption_key,
        StaticSecret::from(random_bytes()?),
        &Opened {
            amount: amount_bytes(amount),
            blinding: scalar_to_be_bytes(blinding),
        },
    )?;
    Ok((
        CreatedNote {
            public_key,
            blinding,
            amount,
        },
        encrypted,
    ))
}

/// The allowlist path of the wallet's key, from the API.
async fn membership(ctx: &Context, wallet: &Wallet) -> Result<Membership> {
    let leaf = scalar_hex(membership_leaf(wallet.note_public_key()));
    let path = ctx.api.allowlist_path(&leaf).await?.with_context(|| {
        format!(
            "{} is not on the allowlist yet: run `veil-demo enroll`",
            wallet.name
        )
    })?;
    Ok(Membership {
        path_elements: path_scalars(&path)?,
        path_indices: path.path_indices,
        root: scalar(&path.root)?,
    })
}

/// The `transact` argument the stellar CLI takes for a proof.
fn proof_json(fixture: &Fixture) -> Result<serde_json::Value> {
    let public = fixture.public_inputs_be();
    ensure!(public.len() == 9, "expected 9 public inputs");
    let (a, b, c) = proof_soroban_parts(&fixture.proof);
    Ok(serde_json::json!({
        "root": decimal(&public[0]),
        "public_amount": decimal(&public[1]),
        "ext_data_hash": hex::encode(public[2]),
        "input_nullifiers": [decimal(&public[3]), decimal(&public[4])],
        "output_commitment0": decimal(&public[5]),
        "output_commitment1": decimal(&public[6]),
        "asp_membership_root": decimal(&public[7]),
        "proof": { "a": hex::encode(a), "b": hex::encode(b), "c": hex::encode(c) },
    }))
}

fn prove(transaction: &Transaction) -> Result<Fixture> {
    let mut rng = StdRng::from_seed(random_bytes()?);
    prove_transaction(transaction, &mut rng)
}

/// Submits `transaction` through `transact` and waits until the API has
/// indexed it (its first nullifier reads as spent).
async fn submit(
    ctx: &Context,
    wallet: &Wallet,
    transaction: &Transaction,
    ext: &ExtData,
) -> Result<Submitted> {
    println!("  proving (Groth16, BN254)...");
    let fixture = prove(transaction)?;
    let nullifier = format!("0x{}", hex::encode(fixture.public_inputs_be()[3]));
    println!("  submitting transact as {}...", wallet.address);
    let submitted = ctx.cli.invoke(
        &ctx.pool,
        &wallet.stellar_alias,
        "transact",
        &[
            ("proof", proof_json(&fixture)?.to_string()),
            ("ext_data", ext.to_cli_json().to_string()),
            ("sender", wallet.address.clone()),
        ],
    )?;
    println!("  waiting for the API to index it...");
    for _ in 0..120 {
        if ctx.api.is_spent(&nullifier).await? {
            return Ok(submitted);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("the API did not show the transaction within four minutes")
}

/// Puts the wallet's key on the allowlist (through the faucet) and publishes
/// its receiving keys, skipping whatever is already done.
pub async fn enroll(ctx: &Context, wallet: &Wallet) -> Result<Vec<Submitted>> {
    let mut done = Vec::new();
    let leaf = membership_leaf(wallet.note_public_key());
    if ctx.api.allowlist_path(&scalar_hex(leaf)).await?.is_none() {
        println!("  enrolling the note key on the allowlist through the faucet...");
        done.push(ctx.cli.invoke(
            &ctx.faucet,
            &wallet.stellar_alias,
            "onboard",
            &[
                ("to", wallet.address.clone()),
                ("membership_leaf", decimal(&scalar_to_be_bytes(leaf))),
            ],
        )?);
    }
    let note_key = hex::encode(scalar_to_be_bytes(wallet.note_public_key()));
    let published = ctx.api.public_key(&wallet.address).await?;
    if published.is_none_or(|key| key.note_key.trim_start_matches("0x") != note_key) {
        println!("  publishing receiving keys...");
        done.push(
            ctx.cli.invoke(
                &ctx.pool,
                &wallet.stellar_alias,
                "register",
                &[(
                    "account",
                    serde_json::json!({
                        "owner": wallet.address,
                        "encryption_key": hex::encode(wallet.encryption_key()),
                        "note_key": note_key,
                    })
                    .to_string(),
                )],
            )?,
        );
    }
    println!("  waiting for the API to index it...");
    for _ in 0..120 {
        let enrolled = ctx.api.allowlist_path(&scalar_hex(leaf)).await?.is_some();
        let keys = ctx.api.public_key(&wallet.address).await?;
        if enrolled && keys.is_some_and(|key| key.note_key.trim_start_matches("0x") == note_key) {
            return Ok(done);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("the API did not show the enrolment within four minutes")
}

/// Finds the wallet's unspent notes by trying to open every note the API
/// lists, checking each against its commitment and the spent set.
pub async fn scan(ctx: &Context, wallet: &Wallet) -> Result<Vec<Owned>> {
    let public_key = wallet.note_public_key();
    let mut owned = Vec::new();
    for note in ctx.api.notes(0).await? {
        let bytes = hex::decode(note.encrypted_output.trim_start_matches("0x"))?;
        let Some(opened) = note_crypto::decrypt(&wallet.encryption_secret, &bytes) else {
            continue;
        };
        // The amount must fit the 248 bits the circuit allows, and u128 here.
        if opened.amount[..16].iter().any(|byte| *byte != 0) {
            continue;
        }
        let mut low = [0u8; 16];
        low.copy_from_slice(&opened.amount[16..]);
        let amount = u128::from_be_bytes(low);
        let blinding = scalar_from_be_bytes(&opened.blinding);
        // A note that does not open its commitment cannot be spent.
        let commitment = note_commitment(
            NoteScalar::from(BigUint::from(amount)),
            public_key,
            blinding,
        );
        if scalar_hex(commitment) != note.commitment {
            println!(
                "  note at {} decrypts but does not match its commitment, ignored",
                note.leaf_index
            );
            continue;
        }
        let nullifier = scalar_hex(note_nullifier(
            wallet.note_private_key,
            commitment,
            note.leaf_index,
        ));
        if amount == 0 || ctx.api.is_spent(&nullifier).await? {
            continue;
        }
        owned.push(Owned {
            leaf_index: note.leaf_index,
            amount,
            blinding,
            nullifier,
        });
    }
    Ok(owned)
}

/// Moves `amount` stroops from the wallet's account into a private note.
pub async fn shield(ctx: &Context, wallet: &Wallet, amount: u128) -> Result<Submitted> {
    ensure!(amount > 0, "the amount must be positive");
    let membership = membership(ctx, wallet).await?;
    let pool = ctx.api.pool().await?;
    let encryption_key = wallet.encryption_key();
    let (note, encrypted) = new_note(wallet.note_public_key(), &encryption_key, amount)?;
    let (zero, encrypted_zero) = new_note(wallet.note_public_key(), &encryption_key, 0)?;
    let ext = ExtData {
        recipient: wallet.address.clone(),
        ext_amount: i128::try_from(amount)?,
        encrypted_output0: encrypted,
        encrypted_output1: encrypted_zero,
    };
    let transaction = Transaction {
        root: scalar(&pool.root)?,
        ext_amount: ext.ext_amount,
        ext_data_hash: ext.hash()?,
        inputs: [dummy_input(wallet)?, dummy_input(wallet)?],
        memberships: [membership.clone(), membership],
        outputs: [note, zero],
    };
    submit(ctx, wallet, &transaction, &ext).await
}

/// Pays `amount` stroops privately to `recipient` (an address that published
/// its keys), with the change back to the wallet.
pub async fn pay(
    ctx: &Context,
    wallet: &Wallet,
    recipient: &str,
    amount: u128,
) -> Result<Submitted> {
    ensure!(amount > 0, "the amount must be positive");
    let keys = ctx
        .api
        .public_key(recipient)
        .await?
        .with_context(|| format!("{recipient} has not published receiving keys"))?;
    let recipient_note_key = scalar(&keys.note_key)?;
    let recipient_encryption: [u8; 32] = hex32(&keys.encryption_key)?;

    // Spend the smallest note that covers the amount, else the two largest.
    let mut notes = scan(ctx, wallet).await?;
    notes.sort_by_key(|note| note.amount);
    let chosen: Vec<Owned> = match notes.iter().find(|note| note.amount >= amount) {
        Some(note) => vec![note.clone()],
        None => notes.iter().rev().take(2).cloned().collect(),
    };
    let available = chosen
        .iter()
        .try_fold(0u128, |sum, note| sum.checked_add(note.amount))
        .context("note amounts overflow")?;
    ensure!(
        available >= amount,
        "the wallet holds {available} stroops privately, less than {amount}"
    );

    // Paths for the spent notes, all against one root.
    let mut paths = Vec::new();
    for note in &chosen {
        paths.push(ctx.api.pool_path(note.leaf_index).await?);
    }
    ensure!(
        paths.windows(2).all(|pair| pair[0].root == pair[1].root),
        "the pool moved while reading paths, try again"
    );
    let root = scalar(&paths[0].root)?;
    let mut inputs = Vec::new();
    for (note, path) in chosen.iter().zip(&paths) {
        inputs.push(SpentNote {
            private_key: wallet.note_private_key,
            blinding: note.blinding,
            amount: note.amount,
            leaf_index: note.leaf_index,
            path_elements: path_scalars(path)?,
        });
    }
    while inputs.len() < 2 {
        inputs.push(dummy_input(wallet)?);
    }
    let membership = membership(ctx, wallet).await?;

    let (payment, encrypted_payment) = new_note(recipient_note_key, &recipient_encryption, amount)?;
    let change_amount = available.checked_sub(amount).context("change underflows")?;
    let (change, encrypted_change) = new_note(
        wallet.note_public_key(),
        &wallet.encryption_key(),
        change_amount,
    )?;
    let ext = ExtData {
        recipient: wallet.address.clone(),
        ext_amount: 0,
        encrypted_output0: encrypted_payment,
        encrypted_output1: encrypted_change,
    };
    let [first, second]: [SpentNote; 2] = inputs
        .try_into()
        .map_err(|_| anyhow::anyhow!("two inputs expected"))?;
    let transaction = Transaction {
        root,
        ext_amount: 0,
        ext_data_hash: ext.hash()?,
        inputs: [first, second],
        memberships: [membership.clone(), membership],
        outputs: [payment, change],
    };
    submit(ctx, wallet, &transaction, &ext).await
}
