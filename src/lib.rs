pub mod auth;
pub mod comfy;
pub mod comfy_monitor;
pub mod config;
pub mod db;
pub mod error;
mod handlers;
mod images;
pub mod logging;
pub mod prompts;
pub mod state;
pub mod worker;
pub mod workflow;

pub use config::Config;
pub use error::AppError;
pub use state::AppState;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{Request, header},
    middleware,
    routing::{get, post},
};
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Multipart upload cap for POST /api/jobs.
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/api/prompts", get(handlers::list_prompts))
        .route(
            "/api/jobs",
            post(handlers::submit_job)
                .get(handlers::list_jobs)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/api/jobs/{id}",
            get(handlers::get_job).delete(handlers::delete_job),
        )
        .route("/api/jobs/{id}/input", get(images::get_input))
        .route("/api/jobs/{id}/result", get(images::get_result))
        .route("/api/jobs/{id}/thumb", get(images::get_thumb))
        .route("/api/debug/job", post(handlers::debug_create_job))
        .route("/api/debug/jobs", get(handlers::debug_list_jobs))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state.clone());

    let app = Router::new()
        .route("/api/health", get(health))
        .with_state(state)
        .merge(authed);

    app.layer(
        ServiceBuilder::new()
            .layer(SetSensitiveRequestHeadersLayer::new(std::iter::once(
                header::AUTHORIZATION,
            )))
            .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
            .layer(
                TraceLayer::new_for_http().make_span_with(|req: &Request<_>| {
                    let id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    tracing::info_span!(
                        "request",
                        id = %id,
                        method = %req.method(),
                        uri = %req.uri(),
                    )
                }),
            )
            .layer(PropagateRequestIdLayer::x_request_id()),
    )
}

/// Liveness + ComfyUI reachability. Unauthenticated — Android uses this
/// to show a connection banner. Doesn't include detailed error strings
/// to avoid leaking internal info from an anonymous endpoint.
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let h = state.comfy_health.read().await;
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "comfy": {
            "ok": h.is_healthy(),
            "last_ok_at": h.last_ok_at,
            "consecutive_failures": h.consecutive_failures,
        }
    }))
}
