//! HTTP-mappable error type. We never leak `anyhow!` chains to clients.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("execution {0} not found")]
    NotFound(String),

    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: String,
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match &self {
            ServerError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            ServerError::BadRequest(_) => {
                (StatusCode::BAD_REQUEST, "bad_request", self.to_string())
            }
            ServerError::Conflict(_) => (StatusCode::CONFLICT, "conflict", self.to_string()),
            // Gemini PR-7 review: never leak `Internal` details to clients —
            // anyhow chains may carry stack traces, file paths, or secrets.
            // Log server-side, return a generic message to the caller.
            ServerError::Internal(detail) => {
                tracing::error!(detail, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error".to_string(),
                )
            }
        };
        (status, Json(ErrorBody { code, message: msg })).into_response()
    }
}

impl From<executor_core::errors::AlgoError> for ServerError {
    fn from(e: executor_core::errors::AlgoError) -> Self {
        ServerError::BadRequest(format!("algo: {e}"))
    }
}

impl From<executor_core::errors::ExecutorError> for ServerError {
    fn from(e: executor_core::errors::ExecutorError) -> Self {
        use executor_core::errors::ExecutorError;
        match e {
            ExecutorError::ExecutionNotFound(s) => ServerError::NotFound(s),
            ExecutorError::ExecutionAlreadyRunning(s) => ServerError::Conflict(s),
            ExecutorError::InvalidRequest(s) => ServerError::BadRequest(s),
            ExecutorError::Algo(a) => ServerError::BadRequest(format!("algo: {a}")),
            ExecutorError::Internal(s) => ServerError::Internal(s),
        }
    }
}
