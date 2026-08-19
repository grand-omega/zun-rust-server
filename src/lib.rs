pub mod auth;
pub mod backup;
pub mod comfy;
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
    http::header,
    middleware,
    routing::{get, post},
};
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::{
    sensitive_headers::SetSensitiveRequestHeadersLayer, timeout::TimeoutLayer, trace::TraceLayer,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Multipart upload cap for POST /api/v1/jobs.
pub(crate) const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Per-request timeout. Bounds slow-loris and stuck handlers; only applies
/// up to the point the handler returns a Response (streaming body bytes
/// after that are not constrained). 120s is generous for the 20 MB upload
/// path on a slow link while still cutting off pathological clients.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Cap on user-supplied prompt text (free-form `prompt_text` and stored
/// `custom_prompts.text`). Generous for natural-language prompts, but
/// keeps the DB and audit logs from absorbing accidental bulk uploads.
pub(crate) const MAX_PROMPT_LEN: usize = 8 * 1024;

/// Caps on the remaining free-text fields a client can store. `text` has had
/// [`MAX_PROMPT_LEN`] from the start; these close the gap for the fields
/// alongside it, so one handler no longer bounds some of its inputs and not
/// others.
pub(crate) const MAX_LABEL_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 1000;
/// `inputs.original_name` — a filename echoed back for display only.
pub(crate) const MAX_INPUT_NAME_LEN: usize = 255;

/// Default per-job timeout for the ComfyUI poll loop. Overridable per
/// custom_prompts row via `timeout_seconds`.
///
/// Sized for a *cold* ComfyUI, not a warm one. Measured on an RTX 4070 Ti
/// Super with FLUX2 klein: ~3 s per job once the model is resident, ~20 s
/// for the first job after a ComfyUI restart (it stages ~4 GB of weights),
/// and ~105 s for that same first job when something else is holding most
/// of the VRAM. 60 s covered the warm case and quietly failed the third
/// one, so the default now leaves room for a cold start under contention.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 120;

/// Accepted range for a per-prompt `timeout_seconds`. Enforced when a
/// prompt is written, and clamped again when a job is read back — an
/// out-of-range value is not merely odd, it wedges the server: the worker
/// runs one job at a time, and a negative value cast to `u64` becomes
/// `u64::MAX`, i.e. a timeout that never fires and a queue that never moves.
pub const MIN_TIMEOUT_SECONDS: i64 = 1;
pub const MAX_TIMEOUT_SECONDS: i64 = 1800;

/// Clamp a stored `timeout_seconds` into the supported range. Applied on the
/// read path so rows written before the range was enforced (or edited
/// straight in SQLite) still produce a timeout that actually fires.
pub fn clamp_timeout_seconds(stored: Option<i64>) -> u64 {
    match stored {
        Some(t) => t.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS) as u64,
        None => DEFAULT_TIMEOUT_SECONDS,
    }
}

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

    // Health is intentionally unauthenticated so external probes
    // (the reverse proxy's upstream check, monitoring, the Android client
    // before it has a token) can verify reachability without a token.
    // It exposes only process liveness and disk usage, no per-job data
    // and no ComfyUI probe.
    // If you want to gate it externally, do that in the proxy
    // (e.g. Caddy `@allowed remote_ip 100.64.0.0/10`).
    let app = Router::new()
        .route("/api/v1/health", get(health))
        .with_state(state)
        .merge(authed);

    app.layer(
        ServiceBuilder::new()
            .layer(SetSensitiveRequestHeadersLayer::new(std::iter::once(
                header::AUTHORIZATION,
            )))
            .layer(TraceLayer::new_for_http())
            .layer(TimeoutLayer::with_status_code(
                axum::http::StatusCode::REQUEST_TIMEOUT,
                REQUEST_TIMEOUT,
            )),
    )
}

async fn capabilities(State(state): State<AppState>) -> Json<serde_json::Value> {
    let workflows = state.workflows.support_list();
    Json(json!({
        "version": VERSION,
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

/// Liveness + cached disk usage. Unauthenticated so external probes and the
/// Android client can verify reachability before they have a token.
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let disk_bytes = compute_or_reuse_disk_usage(&state).await;
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "disk": {
            "data_bytes": disk_bytes,
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
    let dir = state.config.data_dir.clone();
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
