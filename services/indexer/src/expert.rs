//! Stellar Expert's event archive, read once to seed history.
//!
//! Soroban RPC keeps about seven days of events. Stellar Expert keeps them
//! all, with the same topic and body XDR, and pages a contract's events in
//! ledger order with the event id as the cursor. Measured 2026-10-28:
//! `/contract/{id}/events?order=asc&limit=N&cursor=<id>` returns the events
//! after `<id>`, and `/ledger/last` the newest ledger Stellar Expert has
//! processed. Requests without a browser-like user agent are refused.

use std::time::Duration;

use serde::Deserialize;

use crate::rpc::Retry;
use crate::source::ExpertEventRecord;

#[derive(Debug, thiserror::Error)]
pub enum ExpertError {
    #[error("request to {url} failed after {attempts} attempts: {source}")]
    Transport {
        url: String,
        attempts: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} answered HTTP {status} after {attempts} attempts")]
    Status {
        url: String,
        status: u16,
        attempts: u32,
    },
}

#[derive(Clone, Debug)]
pub struct ExpertClient {
    http: reqwest::Client,
    /// e.g. `https://api.stellar.expert/explorer/testnet`
    base: String,
    retry: Retry,
}

#[derive(Deserialize)]
struct EventsResponse {
    #[serde(rename = "_embedded")]
    embedded: Embedded,
}

#[derive(Deserialize)]
struct Embedded {
    records: Vec<ExpertEventRecord>,
}

#[derive(Deserialize)]
struct LastLedger {
    sequence: u32,
}

impl ExpertClient {
    pub fn new(base: &str, timeout: Duration, retry: Retry) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent(concat!(
                "veil-indexer/",
                env!("CARGO_PKG_VERSION"),
                " curl/8"
            ))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            retry,
        })
    }

    /// The newest ledger Stellar Expert has processed.
    pub async fn latest_ledger(&self) -> Result<u32, ExpertError> {
        let last: LastLedger = self.get(&format!("{}/ledger/last", self.base)).await?;
        Ok(last.sequence)
    }

    /// Up to `limit` events of `contract` after `cursor`, oldest first.
    pub async fn events(
        &self,
        contract: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ExpertEventRecord>, ExpertError> {
        let mut url = format!(
            "{}/contract/{contract}/events?order=asc&limit={limit}",
            self.base
        );
        if let Some(cursor) = cursor {
            url.push_str("&cursor=");
            url.push_str(cursor);
        }
        let response: EventsResponse = self.get(&url).await?;
        Ok(response.embedded.records)
    }

    async fn get<R: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<R, ExpertError> {
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let failure = match self.http.get(url).send().await {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .await
                        .map_err(|source| ExpertError::Transport {
                            url: url.to_owned(),
                            attempts: attempt,
                            source,
                        });
                }
                Ok(response) => {
                    let status = response.status();
                    let error = ExpertError::Status {
                        url: url.to_owned(),
                        status: status.as_u16(),
                        attempts: attempt,
                    };
                    if !(status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                    {
                        return Err(error);
                    }
                    error
                }
                Err(source) => ExpertError::Transport {
                    url: url.to_owned(),
                    attempts: attempt,
                    source,
                },
            };
            if attempt >= self.retry.attempts {
                return Err(failure);
            }
            let delay = self.retry.delay(attempt.saturating_sub(1));
            tracing::warn!(attempt, ?delay, error = %failure, "Stellar Expert request failed, retrying");
            tokio::time::sleep(delay).await;
        }
    }
}
