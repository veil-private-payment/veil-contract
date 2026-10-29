//! Errors as the API reports them.
//!
//! Every error is JSON, `{"error": {"code": "...", "message": "..."}}`, with
//! a stable `code` a client can match on. Internal failures are logged with
//! their cause and reported without it, so database details never leak.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    /// The request is valid but the data cannot answer it, such as a Merkle
    /// path for an allowlist its operator maintains off chain.
    #[error("{0}")]
    Conflict(String),
    /// The index is not ready to answer yet, such as before the first
    /// verification recorded the tree depth.
    #[error("{0}")]
    Unavailable(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorDetail {
    /// One of `bad_request`, `not_found`, `conflict`, `unavailable`,
    /// `internal`, `rate_limited`.
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();
        if let Self::Internal(cause) = &self {
            tracing::error!(error = format!("{cause:#}"), "request failed");
        }
        let body = ErrorBody {
            error: ErrorDetail {
                code,
                message: self.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}
