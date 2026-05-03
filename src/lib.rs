pub mod auth;
pub mod backup;
pub mod comfy;
pub mod comfy_monitor;
pub mod config;
pub mod custom_prompts;
pub mod db;
pub mod derived_images;
pub mod error;
mod handlers;
pub mod hash;
mod images;
pub mod inputs;
pub mod logging;
pub mod paths;
pub mod purge;
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

/// Multipart upload cap for POST /api/v1/jobs.
pub(crate) const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Cap on user-supplied prompt text (free-form `prompt_text` and stored
/// `custom_prompts.text`). Generous for natural-language prompts, but
/// keeps the DB and audit logs from absorbing accidental bulk uploads.
pub(crate) const MAX_PROMPT_LEN: usize = 8 * 1024;

/// Default per-job timeout for the ComfyUI poll loop. Overridable per
/// custom_prompts row via `timeout_seconds`.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;

pub fn router(state: AppState) -> Router {
    let authed = Router::new()
        // Custom prompts CRUD
        .route(
            "/api/v1/prompts",
            post(custom_prompts::create).get(custom_prompts::list),
        )
        .route(
            "/api/v1/prompts/{id}",
            get(custom_prompts::get_one)
                .patch(custom_prompts::update)
                .delete(custom_prompts::delete),
        )
        // Jobs
        .route(
            "/api/v1/jobs",
            post(handlers::submit_job)
                .get(handlers::list_jobs)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/api/v1/jobs/{id}",
            get(handlers::get_job).delete(handlers::delete_job),
        )
        .route("/api/v1/jobs/{id}/restore", post(handlers::restore_job))
        .route("/api/v1/jobs/{id}/cancel", post(handlers::cancel_job))
        .route("/api/v1/jobs/{id}/result", get(images::get_result))
        .route("/api/v1/jobs/{id}/thumb", get(images::get_thumb))
        .route("/api/v1/jobs/{id}/preview", get(images::get_preview))
        // Inputs (read-only)
        .route("/api/v1/inputs/{id}", get(inputs::get_input))
        .route("/api/v1/inputs/{id}/file", get(inputs::get_input_file))
        // Server/GPU backend capability report.
        .route("/api/v1/capabilities", get(capabilities))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state.clone());

    let app = Router::new()
        .route("/api/v1/health", get(health))
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

async fn capabilities(State(state): State<AppState>) -> Json<serde_json::Value> {
    let h = state.comfy_health.read().await;
    let workflows = state.workflows.support_list();
    Json(json!({
        "version": VERSION,
        "comfy": {
            "reachable": h.is_healthy(),
            "last_ok_at": h.last_ok_at,
            "consecutive_failures": h.consecutive_failures,
        },
        "features": {
            "image_edit": state.workflows.supported_count() > 0,
            "auto_mask": false,
            "lora": false,
            "reference_image": false,
            "manual_mask": false,
            "text_to_image": false,
        },
        "workflows": workflows,
    }))
}

/// Liveness + ComfyUI reachability + cached disk usage. Unauthenticated —
/// Android uses this to show a connection banner.
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let h = state.comfy_health.read().await;
    let disk_bytes = compute_or_reuse_disk_usage(&state).await;
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "comfy": {
            "ok": h.is_healthy(),
            "last_ok_at": h.last_ok_at,
            "consecutive_failures": h.consecutive_failures,
        },
        "disk": {
            "data_users_bytes": disk_bytes,
        }
    }))
}

const DISK_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

async fn compute_or_reuse_disk_usage(state: &AppState) -> Option<u64> {
    {
        let guard = state.disk_usage_cache.lock();
        if let Some(sample) = *guard
            && sample.computed_at.elapsed() < DISK_CACHE_TTL
        {
            return Some(sample.total_bytes);
        }
    }
    let dir = state.config.data_dir.join("users");
    let total = tokio::task::spawn_blocking(move || dir_size(&dir).unwrap_or(0))
        .await
        .ok()?;
    let mut guard = state.disk_usage_cache.lock();
    *guard = Some(state::DiskUsageSample {
        total_bytes: total,
        computed_at: std::time::Instant::now(),
    });
    Some(total)
}

fn dir_size(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}
