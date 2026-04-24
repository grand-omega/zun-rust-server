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

    #[error("too many requests")]
    TooManyRequests,

    #[error("not found")]
    NotFound,

    #[error("{0}")]
    BadRequest(String),

    #[error("job not ready yet")]
    NotReady,

    /// Submit handler asked for a hash whose file is purged (or never
    /// uploaded). Client should retry as multipart with bytes.
    #[error("input cache miss; re-upload required")]
    NeedUpload { input_id: Option<i64> },

    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

impl AppError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotReady => (StatusCode::CONFLICT, "not_ready"),
            Self::NeedUpload { .. } => (StatusCode::CONFLICT, "need_upload"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        if status.is_server_error() {
            tracing::error!(error = ?self, code, "request failed");
        }
        let body = match &self {
            Self::NeedUpload { input_id } => json!({
                "error": self.to_string(),
                "code": code,
                "need_upload": true,
                "input_id": input_id,
            }),
            // For 5xx, the user-facing string ("internal error") hides what
            // actually went wrong. On a single-user private network we'd
            // rather see the real cause on the phone than have to SSH into
            // the box to read the log. Emit the full anyhow chain.
            Self::Internal(e) => json!({
                "error": format!("{e:#}"),
                "code": code,
            }),
            _ => json!({ "error": self.to_string(), "code": code }),
        };
        (status, Json(body)).into_response()
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

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}
