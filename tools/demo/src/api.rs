//! The read API client. Everything the demo learns about the pool comes
//! through here: roots, paths, spent checks, notes and published keys.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
}

#[derive(Debug, Deserialize)]
pub struct Status {
    pub pool: String,
    pub allowlist: String,
    pub indexed_ledger: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct Pool {
    pub levels: u32,
    pub next_index: u64,
    pub root: String,
}

#[derive(Debug, Deserialize)]
pub struct Path {
    pub leaf_index: u64,
    pub leaf: String,
    pub path_elements: Vec<String>,
    pub path_indices: u64,
    pub root: String,
}

#[derive(Debug, Deserialize)]
pub struct Note {
    pub leaf_index: u64,
    pub commitment: String,
    pub encrypted_output: String,
}

#[derive(Debug, Deserialize)]
struct NotePage {
    items: Vec<Note>,
    next_cursor: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct PublicKey {
    pub encryption_key: String,
    pub note_key: String,
}

#[derive(Debug, Deserialize)]
struct Spent {
    spent: bool,
}

impl Api {
    pub fn new(base: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    /// GETs `path`. `Ok(None)` on 404.
    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<Option<T>> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("GET {url}: {status} {body}");
        }
        Ok(Some(
            response
                .json()
                .await
                .with_context(|| format!("GET {url}"))?,
        ))
    }

    async fn need<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.get(path)
            .await?
            .with_context(|| format!("GET {path}: not found"))
    }

    pub async fn status(&self) -> Result<Status> {
        self.need("/v1/status").await
    }

    pub async fn pool(&self) -> Result<Pool> {
        self.need("/v1/pool").await
    }

    pub async fn pool_path(&self, leaf_index: u64) -> Result<Path> {
        self.need(&format!("/v1/pool/leaves/{leaf_index}/path"))
            .await
    }

    pub async fn allowlist_path(&self, leaf: &str) -> Result<Option<Path>> {
        self.get(&format!("/v1/allowlist/leaves/{leaf}/path")).await
    }

    pub async fn is_spent(&self, nullifier: &str) -> Result<bool> {
        let spent: Spent = self
            .need(&format!("/v1/pool/nullifiers/{nullifier}"))
            .await?;
        Ok(spent.spent)
    }

    pub async fn public_key(&self, owner: &str) -> Result<Option<PublicKey>> {
        self.get(&format!("/v1/public-keys/{owner}")).await
    }

    /// Every note from `cursor` on.
    pub async fn notes(&self, mut cursor: u64) -> Result<Vec<Note>> {
        let mut out = Vec::new();
        loop {
            let page: NotePage = self
                .need(&format!("/v1/notes?cursor={cursor}&limit=1000"))
                .await?;
            out.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = next,
                None => return Ok(out),
            }
        }
    }

    /// Waits until the index has read `ledger`.
    pub async fn wait_for_ledger(&self, ledger: u32) -> Result<()> {
        for _ in 0..120 {
            if self
                .status()
                .await?
                .indexed_ledger
                .is_some_and(|at| at >= ledger)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("the index did not reach ledger {ledger} within four minutes")
    }
}
