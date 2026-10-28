//! Soroban RPC `getEvents`, paged and retried.
//!
//! Behaviour this relies on, measured against testnet RPC on 2026-10-27:
//!
//! - The response `cursor` is an event position. When the page is full it is
//!   the last event returned. Otherwise it is the end of the range the server
//!   scanned, with event index `u32::MAX`.
//! - A request scans at most 10,000 ledgers, so pages with no events still
//!   move the cursor forward and reading to the tip may take several of them.
//! - At the tip the cursor's ledger equals `latestLedger`.
//! - A `startLedger` outside the retained range (about seven days) is refused
//!   with code -32600.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::{DecodeError, RawEvent};
use crate::position::{EventPosition, ParsePositionError};
use crate::source::{ContractEvent, Source};

/// Largest page `getEvents` accepts.
pub const MAX_PAGE_SIZE: u32 = 10_000;

/// Where a `getEvents` request starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// The first ledger to read, for a stream with no checkpoint yet.
    Ledger(u32),
    /// Resume after this position, exclusive.
    After(EventPosition),
}

/// One page of events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsPage {
    pub events: Vec<ContractEvent>,
    /// Pass back as [`Start::After`] to read the next page.
    pub cursor: EventPosition,
    pub latest_ledger: u32,
    pub oldest_ledger: u32,
}

impl EventsPage {
    /// Whether this page reached the newest ledger the server has.
    pub fn at_tip(&self) -> bool {
        self.cursor.ledger() >= self.latest_ledger
    }
}

/// Anything that can serve pages of contract events. [`RpcClient`] is the
/// real one, and tests substitute their own.
pub trait EventSource {
    fn events(
        &self,
        start: Start,
        contracts: &[String],
        limit: u32,
    ) -> impl Future<Output = Result<EventsPage, RpcError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
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
    #[error("RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("RPC response has neither a result nor an error")]
    EmptyResponse,
    #[error("RPC returned event {id} for {contract}, which was not requested")]
    UnexpectedContract { id: String, contract: String },
    #[error("RPC event {id} reports ledger {reported}, but its id is in ledger {from_id}")]
    LedgerMismatch {
        id: String,
        reported: u32,
        from_id: u32,
    },
    #[error(transparent)]
    Position(#[from] ParsePositionError),
    #[error("RPC event {id}: {source}")]
    Decode {
        id: String,
        #[source]
        source: DecodeError,
    },
    #[error("page size {0} is outside 1..={MAX_PAGE_SIZE}")]
    PageSize(u32),
}

impl RpcError {
    /// The server no longer has the requested start: the caller fell further
    /// behind than RPC retention.
    pub fn is_out_of_range(&self) -> bool {
        matches!(self, Self::Rpc { code: -32600, message } if message.contains("ledger range"))
    }
}

/// Exponential backoff for failed requests.
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    pub attempts: u32,
    pub first_delay: Duration,
    pub max_delay: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 6,
            first_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl Retry {
    pub(crate) fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
        self.first_delay
            .checked_mul(factor)
            .map_or(self.max_delay, |delay| delay.min(self.max_delay))
    }
}

#[derive(Clone, Debug)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
    retry: Retry,
}

impl RpcClient {
    pub fn new(url: &str, timeout: Duration, retry: Retry) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent(concat!("veil-indexer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            url: url.to_owned(),
            retry,
        })
    }

    pub(crate) async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R, RpcError> {
        let body = Request {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let failure = match self.http.post(&self.url).json(&body).send().await {
                Ok(response) if response.status().is_success() => {
                    let response: Response<R> =
                        response
                            .json()
                            .await
                            .map_err(|source| RpcError::Transport {
                                url: self.url.clone(),
                                attempts: attempt,
                                source,
                            })?;
                    return match (response.result, response.error) {
                        (_, Some(error)) => Err(RpcError::Rpc {
                            code: error.code,
                            message: error.message,
                        }),
                        (Some(result), None) => Ok(result),
                        (None, None) => Err(RpcError::EmptyResponse),
                    };
                }
                Ok(response) => {
                    let status = response.status();
                    let retryable = status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                    let error = RpcError::Status {
                        url: self.url.clone(),
                        status: status.as_u16(),
                        attempts: attempt,
                    };
                    if !retryable {
                        return Err(error);
                    }
                    error
                }
                Err(source) => RpcError::Transport {
                    url: self.url.clone(),
                    attempts: attempt,
                    source,
                },
            };
            if attempt >= self.retry.attempts {
                return Err(failure);
            }
            let delay = self.retry.delay(attempt.saturating_sub(1));
            tracing::warn!(method, attempt, ?delay, error = %failure, "RPC request failed, retrying");
            tokio::time::sleep(delay).await;
        }
    }
}

impl EventSource for RpcClient {
    async fn events(
        &self,
        start: Start,
        contracts: &[String],
        limit: u32,
    ) -> Result<EventsPage, RpcError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RpcError::PageSize(limit));
        }
        let cursor = match start {
            Start::After(position) => Some(position.to_string()),
            Start::Ledger(_) => None,
        };
        let params = GetEventsParams {
            start_ledger: match start {
                Start::Ledger(ledger) => Some(ledger),
                Start::After(_) => None,
            },
            filters: [Filter {
                kind: "contract",
                contract_ids: contracts,
            }],
            pagination: Pagination { cursor, limit },
        };
        let result: GetEventsResult = self.call("getEvents", &params).await?;
        result.into_page(contracts)
    }
}

#[derive(Serialize)]
struct Request<'a, P> {
    jsonrpc: &'static str,
    id: u32,
    method: &'static str,
    params: &'a P,
}

#[derive(Deserialize)]
struct Response<R> {
    result: Option<R>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: i64,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetEventsParams<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    start_ledger: Option<u32>,
    filters: [Filter<'a>; 1],
    pagination: Pagination,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Filter<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    contract_ids: &'a [String],
}

#[derive(Serialize)]
struct Pagination {
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    limit: u32,
}

/// The `getEvents` result, with the fields the indexer reads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetEventsResult {
    pub events: Vec<RpcEvent>,
    pub cursor: String,
    pub latest_ledger: u32,
    pub oldest_ledger: u32,
}

/// One event as `getEvents` returns it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcEvent {
    pub id: String,
    pub ledger: u32,
    pub contract_id: String,
    pub topic: Vec<String>,
    pub value: String,
    /// Absent on current servers, which only return events from successful
    /// calls. Older servers set it to `false` for events of a failed call,
    /// and those never took effect.
    #[serde(default)]
    pub in_successful_contract_call: Option<bool>,
}

impl GetEventsResult {
    /// Checks and converts the result. `contracts` is the filter that was
    /// sent: an event from any other contract is an error.
    pub fn into_page(self, contracts: &[String]) -> Result<EventsPage, RpcError> {
        let cursor = self.cursor.parse::<EventPosition>()?;
        let mut events = Vec::with_capacity(self.events.len());
        for event in self.events {
            if event.in_successful_contract_call == Some(false) {
                continue;
            }
            if !contracts.contains(&event.contract_id) {
                return Err(RpcError::UnexpectedContract {
                    id: event.id,
                    contract: event.contract_id,
                });
            }
            let position = event.id.parse::<EventPosition>()?;
            if position.ledger() != event.ledger {
                return Err(RpcError::LedgerMismatch {
                    id: event.id,
                    reported: event.ledger,
                    from_id: position.ledger(),
                });
            }
            let raw = RawEvent::from_base64(&event.topic, &event.value).map_err(|source| {
                RpcError::Decode {
                    id: event.id.clone(),
                    source,
                }
            })?;
            events.push(ContractEvent {
                position,
                contract: event.contract_id,
                source: Source::Rpc,
                raw,
            });
        }
        Ok(EventsPage {
            events,
            cursor,
            latest_ledger: self.latest_ledger,
            oldest_ledger: self.oldest_ledger,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let retry = Retry {
            attempts: 10,
            first_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(5),
        };
        let delays: Vec<_> = (0..6).map(|attempt| retry.delay(attempt)).collect();
        assert_eq!(
            delays,
            [500, 1000, 2000, 4000, 5000, 5000].map(Duration::from_millis)
        );
        assert_eq!(retry.delay(u32::MAX), Duration::from_secs(5));
    }

    #[test]
    fn the_tip_is_where_the_cursor_reaches_the_latest_ledger() {
        // Cursors as testnet RPC returned them at the tip, ledger 5,409,699.
        let page = |cursor: &str, latest_ledger| EventsPage {
            events: Vec::new(),
            cursor: cursor.parse().expect("cursor parses"),
            latest_ledger,
            oldest_ledger: 5_288_735,
        };
        assert!(page("0023234484581171199-4294967295", 5_409_699).at_tip());
        assert!(!page("0022757893535170559-4294967295", 5_409_699).at_tip());
    }

    #[test]
    fn retention_errors_are_recognised() {
        let error = RpcError::Rpc {
            code: -32600,
            message: "startLedger must be within the ledger range: 5288735 - 5409694".to_owned(),
        };
        assert!(error.is_out_of_range());
    }
}
