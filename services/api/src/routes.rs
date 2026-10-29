//! Routes, handlers and the middleware around them.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, Method, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Deserialize;
use sqlx::PgPool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::{IntoParams, OpenApi};
use veil_indexer::event::U256Be;
use veil_indexer::position::EventPosition;
use veil_indexer::tree::{Field, IncrementalTree, path, root};

use crate::error::{ApiError, ErrorBody};
use crate::model::{
    AllowlistEntry, AllowlistPage, AllowlistState, Leaf, LeafPage, MerklePath, Note, NotePage,
    NullifierStatus, OnChain, PoolState, PublicKey, Status,
};
use crate::queries::{self, hex};

/// Largest page any listing returns.
pub const MAX_PAGE: u64 = 1_000;
const DEFAULT_PAGE: u64 = 100;

#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub network_passphrase: String,
    pub pool: String,
    pub allowlist: String,
    /// `/ready` fails when the indexer has not advanced for this long.
    pub max_staleness: Duration,
    pub request_timeout: Duration,
    /// Sustained requests per second per client IP, and the burst above it.
    /// `None` disables rate limiting (tests).
    pub rate_limit: Option<(u64, u32)>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<ApiConfig>,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Veil read API",
        description = "Read-only access to the Veil pool and allowlist as indexed from Stellar. \
            The server holds no secret and signs nothing. Field elements are 0x-prefixed \
            32-byte big-endian hex."
    ),
    paths(
        status,
        pool_state,
        pool_leaves,
        pool_path,
        nullifier,
        notes,
        public_key,
        allowlist_state,
        allowlist_entries,
        allowlist_path
    ),
    components(schemas(
        Status,
        PoolState,
        OnChain,
        Leaf,
        LeafPage,
        MerklePath,
        NullifierStatus,
        Note,
        NotePage,
        PublicKey,
        AllowlistState,
        AllowlistEntry,
        AllowlistPage,
        ErrorBody
    ))
)]
pub struct ApiDoc;

/// The application, with every middleware but rate limiting, which needs the
/// client address and is added in [`app`].
pub fn routes(state: AppState) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    let timeout = state.config.request_timeout;
    let v1 = Router::new()
        .route("/status", get(status))
        .route("/pool", get(pool_state))
        .route("/pool/leaves", get(pool_leaves))
        .route("/pool/leaves/{leaf_index}/path", get(pool_path))
        .route("/pool/nullifiers/{nullifier}", get(nullifier))
        .route("/notes", get(notes))
        .route("/public-keys/{owner}", get(public_key))
        .route("/allowlist", get(allowlist_state))
        .route("/allowlist/entries", get(allowlist_entries))
        .route("/allowlist/leaves/{leaf}/path", get(allowlist_path));

    Router::new()
        .nest("/v1", v1)
        .route("/openapi.json", get(openapi))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .fallback(not_found)
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET])
                .allow_headers(Any),
        )
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        ))
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}

/// [`routes`] plus per-client rate limiting, keyed by the forwarded client
/// address behind a proxy, else the peer address.
pub fn app(state: AppState) -> anyhow::Result<Router> {
    let limit = state.config.rate_limit;
    let router = routes(state);
    let Some((per_second, burst)) = limit else {
        return Ok(router);
    };
    let config = tower_governor::governor::GovernorConfigBuilder::default()
        .per_millisecond(1_000u64.checked_div(per_second.max(1)).unwrap_or(1).max(1))
        .burst_size(burst.max(1))
        .key_extractor(tower_governor::key_extractor::SmartIpKeyExtractor)
        .finish()
        .ok_or_else(|| anyhow::anyhow!("invalid rate limit"))?;
    let config = Arc::new(config);
    let limiter = config.limiter().clone();
    // Forget idle clients so the limiter's memory stays bounded.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            limiter.retain_recent();
        }
    });
    Ok(router.layer(tower_governor::GovernorLayer { config }))
}

async fn not_found() -> ApiError {
    ApiError::NotFound("no such route".to_owned())
}

async fn health() -> &'static str {
    "ok"
}

/// Ready when the database answers and the indexer advanced recently.
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let age: Result<Option<f64>, sqlx::Error> = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM now() - updated_at)::float8 FROM ingest_checkpoints \
         WHERE stream = 'rpc'",
    )
    .fetch_optional(&state.db)
    .await;
    let limit = state.config.max_staleness.as_secs_f64();
    let (code, reason) = match age {
        Err(err) => {
            tracing::warn!(error = %err, "readiness: database unreachable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("database unreachable".to_owned()),
            )
        }
        Ok(None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("the indexer has not started following RPC".to_owned()),
        ),
        Ok(Some(age)) if age > limit => (
            StatusCode::SERVICE_UNAVAILABLE,
            Some(format!("the indexer last advanced {age:.0}s ago")),
        ),
        Ok(Some(_)) => (StatusCode::OK, None),
    };
    (
        code,
        Json(serde_json::json!({ "ready": reason.is_none(), "reason": reason })),
    )
}

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

fn parse_u256(value: &str, what: &str) -> Result<U256Be, ApiError> {
    let digits = value.strip_prefix("0x").unwrap_or(value);
    let bad = || ApiError::BadRequest(format!("{what} must be 0x followed by 64 hex digits"));
    if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(digits.as_bytes().chunks(2)) {
        let text = std::str::from_utf8(pair).map_err(|_| bad())?;
        *slot = u8::from_str_radix(text, 16).map_err(|_| bad())?;
    }
    Ok(U256Be(out))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IndexPage {
    /// First leaf index to return (inclusive). Default 0.
    cursor: Option<u64>,
    /// Items per page, 1 to 1000. Default 100.
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventPage {
    /// Return changes after this event position. Default: from the start.
    cursor: Option<String>,
    /// Items per page, 1 to 1000. Default 100.
    limit: Option<u64>,
}

fn page_size(limit: Option<u64>) -> Result<u64, ApiError> {
    match limit.unwrap_or(DEFAULT_PAGE) {
        limit @ 1..=MAX_PAGE => Ok(limit),
        _ => Err(ApiError::BadRequest(format!(
            "limit must be 1 to {MAX_PAGE}"
        ))),
    }
}

/// Where the index stands and which contracts it follows.
#[utoipa::path(get, path = "/v1/status", responses((status = 200, body = Status)))]
async fn status(State(state): State<AppState>) -> Result<Json<Status>, ApiError> {
    let mut connection = state.db.acquire().await?;
    let checkpoint = queries::checkpoint(&mut connection).await?;
    Ok(Json(Status {
        network_passphrase: state.config.network_passphrase.clone(),
        pool: state.config.pool.clone(),
        allowlist: state.config.allowlist.clone(),
        indexed_ledger: checkpoint.as_ref().map(|(ledger, _, _)| *ledger),
        latest_ledger: checkpoint.as_ref().map(|(_, latest, _)| *latest),
        updated_at: checkpoint.map(|(_, _, at)| at),
    }))
}

/// The pool's tree: depth, size and root computed from the index, beside the
/// state last read from the contract.
#[utoipa::path(get, path = "/v1/pool", responses(
    (status = 200, body = PoolState),
    (status = 503, description = "Not verified yet", body = ErrorBody)
))]
async fn pool_state(State(state): State<AppState>) -> Result<Json<PoolState>, ApiError> {
    let pool = &state.config.pool;
    let mut tx = queries::snapshot(&state.db).await?;
    let levels = queries::levels(&mut tx, pool).await?;
    let leaves = queries::pool_leaves(&mut tx, pool, 0, None).await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    let on_chain = queries::on_chain(&mut tx, pool)
        .await?
        .map(|(state, _)| state);
    drop(tx);

    let tree = queries::as_tree(&leaves)?;
    let computed = root(levels, &tree).map_err(|err| ApiError::Internal(err.into()))?;
    Ok(Json(PoolState {
        contract: pool.clone(),
        levels,
        next_index: leaves.last().map_or(0, |leaf| leaf.index.saturating_add(1)),
        root: computed.to_u256().to_string(),
        indexed_ledger,
        on_chain,
    }))
}

/// Every position of the pool's tree, in order. Placing each `commitment` at
/// its `leaf_index` (and empty positions at the contracts' zero values)
/// rebuilds the pool's tree.
#[utoipa::path(get, path = "/v1/pool/leaves", params(IndexPage), responses(
    (status = 200, body = LeafPage),
    (status = 400, body = ErrorBody)
))]
async fn pool_leaves(
    State(state): State<AppState>,
    Query(page): Query<IndexPage>,
) -> Result<Json<LeafPage>, ApiError> {
    let limit = page_size(page.limit)?;
    let from = page.cursor.unwrap_or(0);
    let mut tx = queries::snapshot(&state.db).await?;
    let mut leaves = queries::pool_leaves(
        &mut tx,
        &state.config.pool,
        from,
        Some(limit.saturating_add(1)),
    )
    .await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);

    let more = u64::try_from(leaves.len()).unwrap_or(u64::MAX) > limit;
    leaves.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = more
        .then(|| leaves.last().map(|leaf| leaf.index.saturating_add(1)))
        .flatten();
    Ok(Json(LeafPage {
        items: leaves
            .into_iter()
            .map(|leaf| Leaf {
                leaf_index: leaf.index,
                commitment: leaf.value.to_string(),
                origin: leaf.origin.to_owned(),
                ledger: leaf.ledger,
            })
            .collect(),
        next_cursor,
        indexed_ledger,
    }))
}

/// The Merkle path of one pool leaf against the root computed from the index.
#[utoipa::path(get, path = "/v1/pool/leaves/{leaf_index}/path",
    params(("leaf_index" = u64, Path, description = "Tree position")),
    responses(
        (status = 200, body = MerklePath),
        (status = 404, body = ErrorBody),
        (status = 503, body = ErrorBody)
    ))]
async fn pool_path(
    State(state): State<AppState>,
    Path(leaf_index): Path<u64>,
) -> Result<Json<MerklePath>, ApiError> {
    let pool = &state.config.pool;
    let mut tx = queries::snapshot(&state.db).await?;
    let levels = queries::levels(&mut tx, pool).await?;
    let leaves = queries::pool_leaves(&mut tx, pool, 0, None).await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);

    let tree = queries::as_tree(&leaves)?;
    let proof = path(levels, &tree, leaf_index)
        .map_err(|err| ApiError::Internal(err.into()))?
        .ok_or_else(|| ApiError::NotFound(format!("no leaf at index {leaf_index}")))?;
    Ok(Json(merkle_path(&proof, indexed_ledger)))
}

fn merkle_path(proof: &veil_indexer::tree::MerklePath, indexed_ledger: Option<u32>) -> MerklePath {
    MerklePath {
        leaf_index: proof.index,
        leaf: proof.leaf.to_u256().to_string(),
        path_elements: proof
            .siblings
            .iter()
            .map(|sibling| sibling.to_u256().to_string())
            .collect(),
        path_indices: proof.path_indices(),
        root: proof.root.to_u256().to_string(),
        indexed_ledger,
    }
}

/// Whether a nullifier has been spent in the pool.
#[utoipa::path(get, path = "/v1/pool/nullifiers/{nullifier}",
    params(("nullifier" = String, Path, description = "0x-prefixed 32-byte hex")),
    responses((status = 200, body = NullifierStatus), (status = 400, body = ErrorBody)))]
async fn nullifier(
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<NullifierStatus>, ApiError> {
    let parsed = parse_u256(&value, "nullifier")?;
    let mut tx = queries::snapshot(&state.db).await?;
    let spend = queries::nullifier(&mut tx, &state.config.pool, &parsed).await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);
    Ok(Json(NullifierStatus {
        nullifier: parsed.to_string(),
        spent: spend.is_some(),
        ledger: spend.as_ref().map(|(ledger, _)| *ledger),
        event: spend.map(|(_, event)| event),
        indexed_ledger,
    }))
}

/// Encrypted notes in tree order, for recipients to scan from where they
/// last stopped.
#[utoipa::path(get, path = "/v1/notes", params(IndexPage), responses(
    (status = 200, body = NotePage),
    (status = 400, body = ErrorBody)
))]
async fn notes(
    State(state): State<AppState>,
    Query(page): Query<IndexPage>,
) -> Result<Json<NotePage>, ApiError> {
    let limit = page_size(page.limit)?;
    let mut tx = queries::snapshot(&state.db).await?;
    let mut rows = queries::notes(
        &mut tx,
        &state.config.pool,
        page.cursor.unwrap_or(0),
        limit.saturating_add(1),
    )
    .await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);

    let more = u64::try_from(rows.len()).unwrap_or(u64::MAX) > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = more
        .then(|| rows.last().map(|(index, ..)| index.saturating_add(1)))
        .flatten();
    Ok(Json(NotePage {
        items: rows
            .into_iter()
            .map(|(leaf_index, commitment, note, ledger)| Note {
                leaf_index,
                commitment: commitment.to_string(),
                encrypted_output: hex(&note),
                ledger,
            })
            .collect(),
        next_cursor,
        indexed_ledger,
    }))
}

/// The receiving keys an account last published.
#[utoipa::path(get, path = "/v1/public-keys/{owner}",
    params(("owner" = String, Path, description = "Account (G…) or contract (C…) address")),
    responses(
        (status = 200, body = PublicKey),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody)
    ))]
async fn public_key(
    State(state): State<AppState>,
    Path(owner): Path<String>,
) -> Result<Json<PublicKey>, ApiError> {
    match stellar_strkey::Strkey::from_string(&owner) {
        Ok(stellar_strkey::Strkey::PublicKeyEd25519(_) | stellar_strkey::Strkey::Contract(_)) => {}
        _ => {
            return Err(ApiError::BadRequest(
                "owner must be an account (G…) or contract (C…) address".to_owned(),
            ));
        }
    }
    let mut connection = state.db.acquire().await?;
    let (encryption_key, note_key, ledger, registrations) =
        queries::public_key(&mut connection, &state.config.pool, &owner)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("{owner} has not registered keys")))?;
    Ok(Json(PublicKey {
        owner,
        encryption_key: hex(&encryption_key),
        note_key: hex(&note_key),
        ledger,
        registrations,
    }))
}

/// The allowlist: depth, size, latest root and the state last read from the
/// contract.
#[utoipa::path(get, path = "/v1/allowlist", responses(
    (status = 200, body = AllowlistState),
    (status = 503, body = ErrorBody)
))]
async fn allowlist_state(State(state): State<AppState>) -> Result<Json<AllowlistState>, ApiError> {
    let contract = &state.config.allowlist;
    let mut tx = queries::snapshot(&state.db).await?;
    let levels = queries::levels(&mut tx, contract).await?;
    let rows = queries::allowlist(&mut tx, contract, None, None).await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    let on_chain = queries::on_chain(&mut tx, contract)
        .await?
        .map(|(state, _)| state);
    drop(tx);

    let revocations = rows.iter().filter(|row| row.kind == "revoked").count();
    Ok(Json(AllowlistState {
        contract: contract.clone(),
        levels,
        operator_maintained: revocations > 0,
        additions: u64::try_from(rows.len().saturating_sub(revocations)).unwrap_or(u64::MAX),
        revocations: u64::try_from(revocations).unwrap_or(u64::MAX),
        root: rows.last().map(|row| row.root.to_string()),
        indexed_ledger,
        on_chain,
    }))
}

/// Allowlist additions and revocations in the order they happened.
#[utoipa::path(get, path = "/v1/allowlist/entries", params(EventPage), responses(
    (status = 200, body = AllowlistPage),
    (status = 400, body = ErrorBody)
))]
async fn allowlist_entries(
    State(state): State<AppState>,
    Query(page): Query<EventPage>,
) -> Result<Json<AllowlistPage>, ApiError> {
    let limit = page_size(page.limit)?;
    let after = page
        .cursor
        .as_deref()
        .map(|cursor| {
            cursor
                .parse::<EventPosition>()
                .map_err(|_| ApiError::BadRequest("cursor must be an event position".to_owned()))
        })
        .transpose()?;
    let mut tx = queries::snapshot(&state.db).await?;
    let mut rows = queries::allowlist(
        &mut tx,
        &state.config.allowlist,
        after,
        Some(limit.saturating_add(1)),
    )
    .await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);

    let more = u64::try_from(rows.len()).unwrap_or(u64::MAX) > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = more
        .then(|| rows.last().map(|row| row.position.to_string()))
        .flatten();
    Ok(Json(AllowlistPage {
        items: rows
            .into_iter()
            .map(|row| AllowlistEntry {
                kind: row.kind,
                leaf: row.leaf.to_string(),
                leaf_index: row.leaf_index,
                root: row.root.to_string(),
                ledger: row.ledger,
                event: row.position.to_string(),
            })
            .collect(),
        next_cursor,
        indexed_ledger,
    }))
}

/// The membership path of an allowlist leaf. Computed by replaying the
/// additions, and only served when the replayed root equals the root the
/// contract last emitted.
#[utoipa::path(get, path = "/v1/allowlist/leaves/{leaf}/path",
    params(("leaf" = String, Path, description = "0x-prefixed 32-byte hex")),
    responses(
        (status = 200, body = MerklePath),
        (status = 400, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "Operator-maintained allowlist", body = ErrorBody)
    ))]
async fn allowlist_path(
    State(state): State<AppState>,
    Path(value): Path<String>,
) -> Result<Json<MerklePath>, ApiError> {
    let wanted = parse_u256(&value, "leaf")?;
    let contract = &state.config.allowlist;
    let mut tx = queries::snapshot(&state.db).await?;
    let levels = queries::levels(&mut tx, contract).await?;
    let rows = queries::allowlist(&mut tx, contract, None, None).await?;
    let indexed_ledger = queries::indexed_ledger(&mut tx).await?;
    drop(tx);

    if rows.iter().any(|row| row.kind == "revoked") {
        return Err(ApiError::Conflict(
            "this allowlist is maintained by its operator since a revocation: its tree is \
             rebuilt off chain, so paths cannot be derived from its events"
                .to_owned(),
        ));
    }
    let mut incremental =
        IncrementalTree::new(levels).map_err(|err| ApiError::Internal(err.into()))?;
    let mut leaves = Vec::with_capacity(rows.len());
    for row in &rows {
        let field = Field::from_u256(&row.leaf).map_err(|err| ApiError::Internal(err.into()))?;
        if row.leaf_index != Some(incremental.next_index()) {
            return Err(ApiError::Internal(anyhow::anyhow!(
                "allowlist additions are not contiguous at {}",
                row.position
            )));
        }
        incremental
            .push(field)
            .map_err(|err| ApiError::Internal(err.into()))?;
        leaves.push((incremental.next_index().saturating_sub(1), field));
    }
    if let Some(last) = rows.last()
        && incremental.root().to_u256() != last.root
    {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "replayed allowlist root does not match the last emitted root"
        )));
    }
    let index = rows
        .iter()
        .rev()
        .find(|row| row.leaf == wanted)
        .and_then(|row| row.leaf_index)
        .ok_or_else(|| ApiError::NotFound(format!("{wanted} is not on the allowlist")))?;
    let proof = path(levels, &leaves, index)
        .map_err(|err| ApiError::Internal(err.into()))?
        .ok_or_else(|| ApiError::NotFound(format!("{wanted} is not on the allowlist")))?;
    Ok(Json(merkle_path(&proof, indexed_ledger)))
}
