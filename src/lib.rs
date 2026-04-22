pub mod config;
pub mod db;
mod handlers;
pub mod state;

pub use config::Config;
pub use state::AppState;

use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::json;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/debug/job", post(handlers::debug_create_job))
        .route("/api/debug/jobs", get(handlers::debug_list_jobs))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}
