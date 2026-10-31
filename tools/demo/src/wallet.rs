//! A local testnet wallet.
//!
//! Holds the two keys only its owner may know: the note private key (spends
//! notes) and the X25519 secret (opens notes sent to it). The Stellar account
//! that pays fees is kept by the `stellar` CLI under an alias, so its secret
//! never passes through this tool. Wallet files are written owner-readable
//! only. This is for testnet: nothing here is meant to guard real value.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use policy_fixture::{NoteScalar, note_public_key, scalar_from_be_bytes, scalar_to_be_bytes};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Serialize, Deserialize)]
struct WalletFile {
    name: String,
    stellar_alias: String,
    address: String,
    note_private_key: String,
    encryption_secret: String,
}

pub struct Wallet {
    pub name: String,
    /// The `stellar` CLI identity that signs this wallet's transactions.
    pub stellar_alias: String,
    pub address: String,
    pub note_private_key: NoteScalar,
    pub encryption_secret: StaticSecret,
}

/// 32 bytes from the operating system's CSPRNG.
pub fn random_bytes() -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    getrandom::getrandom(&mut out).map_err(|err| anyhow::anyhow!("no randomness: {err}"))?;
    Ok(out)
}

/// A uniformly random field element (64 random bytes reduced mod p).
pub fn random_scalar() -> Result<NoteScalar> {
    use ark_ff::PrimeField;
    let mut wide = [0u8; 64];
    wide[..32].copy_from_slice(&random_bytes()?);
    wide[32..].copy_from_slice(&random_bytes()?);
    Ok(NoteScalar::from_be_bytes_mod_order(&wide))
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

impl Wallet {
    pub fn create(name: &str, stellar_alias: String, address: String) -> Result<Self> {
        Ok(Self {
            name: name.to_owned(),
            stellar_alias,
            address,
            note_private_key: random_scalar()?,
            encryption_secret: StaticSecret::from(random_bytes()?),
        })
    }

    pub fn note_public_key(&self) -> NoteScalar {
        note_public_key(self.note_private_key)
    }

    pub fn encryption_key(&self) -> [u8; 32] {
        PublicKey::from(&self.encryption_secret).to_bytes()
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = path(dir, &self.name);
        ensure!(!file.exists(), "{} already exists", file.display());
        let body = serde_json::to_vec_pretty(&WalletFile {
            name: self.name.clone(),
            stellar_alias: self.stellar_alias.clone(),
            address: self.address.clone(),
            note_private_key: hex::encode(scalar_to_be_bytes(self.note_private_key)),
            encryption_secret: hex::encode(self.encryption_secret.to_bytes()),
        })?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options
            .open(&file)
            .with_context(|| format!("writing {}", file.display()))?;
        handle.write_all(&body)?;
        Ok(file)
    }

    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        let file = path(dir, name);
        let text = fs::read_to_string(&file)
            .with_context(|| format!("reading {} (run `veil-demo new {name}`)", file.display()))?;
        let stored: WalletFile = serde_json::from_str(&text)?;
        let key: [u8; 32] = hex::decode(&stored.note_private_key)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("note key is not 32 bytes"))?;
        let secret: [u8; 32] = hex::decode(&stored.encryption_secret)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("encryption secret is not 32 bytes"))?;
        Ok(Self {
            name: stored.name,
            stellar_alias: stored.stellar_alias,
            address: stored.address,
            note_private_key: scalar_from_be_bytes(&key),
            encryption_secret: StaticSecret::from(secret),
        })
    }
}
