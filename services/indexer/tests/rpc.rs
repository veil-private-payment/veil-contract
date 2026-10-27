//! The `getEvents` client over HTTP, against a local mock server.
//!
//! `fixtures/rpc/get_events_native_sac.json` is a real testnet response
//! recorded on 2026-10-27 (two events of the native asset contract), used for
//! the response shape. The pool had no events within RPC retention that day.

use std::time::Duration;

use serde_json::{Value, json};
use veil_indexer::event::DecodeError;
use veil_indexer::rpc::{EventSource, Retry, RpcClient, RpcError, Start};
use veil_indexer::source::Source;
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NATIVE_SAC: &str = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC";

fn recorded() -> Value {
    let path = format!(
        "{}/tests/fixtures/rpc/get_events_native_sac.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture exists")).expect("JSON")
}

fn client(server: &MockServer) -> RpcClient {
    let retry = Retry {
        attempts: 3,
        first_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };
    RpcClient::new(&server.uri(), Duration::from_secs(5), retry).expect("client builds")
}

fn sac() -> Vec<String> {
    vec![NATIVE_SAC.to_owned()]
}

#[tokio::test]
async fn a_recorded_response_becomes_a_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "method": "getEvents",
            "params": {
                "startLedger": 5_409_644,
                "filters": [{ "type": "contract", "contractIds": [NATIVE_SAC] }],
                "pagination": { "limit": 2 }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded()))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .events(Start::Ledger(5_409_644), &sac(), 2)
        .await
        .expect("page");
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.cursor.to_string(), "0023234244063002624-0000000001");
    assert_eq!(page.latest_ledger, 5_409_694);
    assert_eq!(page.oldest_ledger, 5_288_735);
    assert!(!page.at_tip());
    for event in &page.events {
        assert_eq!(event.source, Source::Rpc);
        assert_eq!(event.contract, NATIVE_SAC);
        assert_eq!(event.position.ledger(), 5_409_644);
        // A token event is not a Veil event, and the decoder says so.
        assert!(matches!(
            event.raw.decode(),
            Err(DecodeError::UnknownEvent { .. })
        ));
    }
}

#[tokio::test]
async fn resuming_sends_the_cursor_and_no_start_ledger() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "params": { "pagination": { "cursor": "0023234244063002624-0000000001", "limit": 2 } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded()))
        .expect(1)
        .mount(&server)
        .await;

    let cursor = "0023234244063002624-0000000001".parse().expect("parses");
    client(&server)
        .events(Start::After(cursor), &sac(), 2)
        .await
        .expect("page");
    let requests = server.received_requests().await.expect("recorded");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("JSON body");
    assert!(body["params"].get("startLedger").is_none());
}

#[tokio::test]
async fn server_errors_are_retried_until_one_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded()))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .events(Start::Ledger(5_409_644), &sac(), 2)
        .await
        .expect("third attempt succeeds");
    assert_eq!(page.events.len(), 2);
}

#[tokio::test]
async fn retries_stop_after_the_configured_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .expect(3)
        .mount(&server)
        .await;

    let error = client(&server)
        .events(Start::Ledger(1), &sac(), 2)
        .await
        .expect_err("gives up");
    assert!(
        matches!(
            error,
            RpcError::Status {
                status: 429,
                attempts: 3,
                ..
            }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn rpc_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32600,
                "message": "startLedger must be within the ledger range: 5288735 - 5409694"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server)
        .events(Start::Ledger(1_000), &sac(), 2)
        .await
        .expect_err("refused");
    assert!(error.is_out_of_range(), "{error}");
}

#[tokio::test]
async fn an_event_outside_the_filter_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded()))
        .mount(&server)
        .await;

    let other = vec!["CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G".to_owned()];
    let error = client(&server)
        .events(Start::Ledger(5_409_644), &other, 2)
        .await
        .expect_err("refused");
    assert!(
        matches!(error, RpcError::UnexpectedContract { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_page_size_rpc_would_refuse_is_caught_before_sending() {
    let server = MockServer::start().await;
    let rpc = client(&server);
    for limit in [0, 10_001] {
        let error = rpc
            .events(Start::Ledger(1), &sac(), limit)
            .await
            .expect_err("refused");
        assert!(matches!(error, RpcError::PageSize(_)), "{error}");
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}
