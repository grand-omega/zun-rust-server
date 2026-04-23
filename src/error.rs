use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,

    #[error("not found")]
    NotFound,

    #[error("{0}")]
    BadRequest(String),

    #[error("unknown prompt id: {0}")]
    UnknownPrompt(String),

    #[error("job not ready yet")]
    NotReady,

    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

impl AppError {
    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::UnknownPrompt(_) => (StatusCode::BAD_REQUEST, "invalid_prompt_id"),
            Self::NotReady => (StatusCode::CONFLICT, "not_ready"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();
        if status.is_server_error() {
            // Debug on anyhow::Error prints the full cause chain — the top
            // message alone is rarely enough to debug a 500.
            tracing::error!(error = ?self, code, "request failed");
        }
        let body = Json(json!({ "error": self.to_string(), "code": code }));
        (status, body).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.into())
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        Self::BadRequest(format!("invalid multipart: {e}"))
    }
}
