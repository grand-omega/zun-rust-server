pub mod auth;
pub mod config;
pub mod db;
pub mod error;
mod handlers;
pub mod prompts;
pub mod state;
pub mod workflow;

pub use config::Config;
pub use error::AppError;
pub use state::AppState;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
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
            post(handlers::submit_job).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/api/jobs/{id}", get(handlers::get_job))
        .route("/api/debug/job", post(handlers::debug_create_job))
        .route("/api/debug/jobs", get(handlers::debug_list_jobs))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state);

    let app = Router::new()
        .route("/api/health", get(health))
        .merge(authed);

    // Middleware stack applied to every route:
    // 1. Mark `Authorization` sensitive so header-debug redacts it
    //    (defense-in-depth — default TraceLayer doesn't log headers,
    //    but custom loggers added later will respect the mark).
    // 2. Generate an `x-request-id` if the client didn't send one.
    // 3. Open a `request` span per request with method/uri/id — every
    //    `tracing::*` call inside a handler inherits these fields.
    // 4. Copy the id onto the response so clients can correlate.
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

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}
