use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::{AppError, AppState};

/// Constant-time bytes comparison to avoid trivial timing side channels.
fn token_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Rejects any request missing or presenting a non-matching
/// `Authorization: Bearer <token>` header.
pub async fn require_bearer(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;

    if !token_eq(presented, &state.config.token) {
        return Err(AppError::Unauthorized);
    }

    Ok(next.run(req).await)
}
