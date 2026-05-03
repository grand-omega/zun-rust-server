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
            // Always log the unredacted chain — operators need full info.
            tracing::error!(error = ?self, code, "request failed");
        }
        let body = match &self {
            Self::NeedUpload { input_id } => json!({
                "error": self.to_string(),
                "code": code,
                "need_upload": true,
                "input_id": input_id,
            }),
            // For 5xx, expose enough of the chain to debug from the phone
            // but redact filesystem paths and upstream URLs.
            Self::Internal(e) => json!({
                "error": redact_internal(&format!("{e:#}")),
                "code": code,
            }),
            _ => json!({ "error": self.to_string(), "code": code }),
        };
        (status, Json(body)).into_response()
    }
}

/// Redact filesystem paths (`/foo/...`) and URLs (`http(s)://...`) from a
/// string. Truncate to a reasonable cap. Hand-rolled — no regex dep — and
/// good enough for the kinds of errors anyhow chains produce.
fn redact_internal(s: &str) -> String {
    const MAX_LEN: usize = 400;
    // Token-by-token: split on whitespace, replace tokens that start with a
    // known sensitive prefix (after stripping leading punctuation).
    let mut out = String::with_capacity(s.len().min(MAX_LEN));
    for (i, tok) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&redact_token(tok));
        if out.len() >= MAX_LEN {
            out.truncate(MAX_LEN);
            out.push('…');
            break;
        }
    }
    out
}

fn redact_token(tok: &str) -> String {
    // Strip leading bracket/quote chars so "(/tmp/foo.jpg)" matches "/tmp/...".
    let leading_len = tok
        .chars()
        .take_while(|c| matches!(c, '(' | '[' | '{' | '"' | '\'' | '<'))
        .map(char::len_utf8)
        .sum::<usize>();
    let (lead, rest) = tok.split_at(leading_len);
    // And strip trailing punctuation so "/tmp/foo.jpg," matches "/tmp/...".
    let trailing_len = rest
        .chars()
        .rev()
        .take_while(|c| {
            matches!(
                c,
                ')' | ']' | '}' | '"' | '\'' | '>' | ',' | ';' | '.' | ':'
            )
        })
        .map(char::len_utf8)
        .sum::<usize>();
    let core = &rest[..rest.len().saturating_sub(trailing_len)];
    let trail = &rest[rest.len().saturating_sub(trailing_len)..];

    let replacement = if is_redactable_path(core) {
        Some("<path>")
    } else if is_redactable_url(core) {
        Some("<url>")
    } else {
        None
    };
    match replacement {
        Some(r) => format!("{lead}{r}{trail}"),
        None => tok.to_string(),
    }
}

fn is_redactable_path(s: &str) -> bool {
    // Unix absolute paths starting with a known top-level dir.
    const ROOTS: &[&str] = &[
        "/home/", "/tmp/", "/var/", "/usr/", "/opt/", "/etc/", "/root/", "/srv/", "/mnt/", "/data/",
    ];
    ROOTS.iter().any(|r| s.starts_with(r))
}

fn is_redactable_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ws://")
        || s.starts_with("wss://")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_paths() {
        let s = "read /home/doremy/Desktop/zun-rust-server/data/cache/inputs/abc.jpg: No such file";
        let r = redact_internal(s);
        assert!(r.contains("<path>"), "got: {r}");
        assert!(!r.contains("/home/"), "leaked path: {r}");
    }

    #[test]
    fn redacts_urls() {
        let s = "comfyui error from http://127.0.0.1:8188/prompt: 500";
        let r = redact_internal(s);
        assert!(r.contains("<url>"), "got: {r}");
        assert!(!r.contains("127.0.0.1"), "leaked url: {r}");
    }

    #[test]
    fn keeps_useful_text() {
        let s = "comfyui timeout after 60s";
        assert_eq!(redact_internal(s), s);
    }

    #[test]
    fn handles_path_in_parens() {
        // Real-world: anyhow chains often quote paths.
        let s = "could not open (/tmp/foo.jpg): permission denied";
        let r = redact_internal(s);
        assert!(r.contains("(<path>)"), "got: {r}");
    }

    #[test]
    fn truncates_long_messages() {
        let s = "x ".repeat(500);
        let r = redact_internal(&s);
        // 400 char cap + UTF-8 encoding of '…' (3 bytes).
        assert!(r.len() <= 403, "len was {}", r.len());
    }
}
