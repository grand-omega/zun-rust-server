pub mod auth;
pub mod config;
pub mod db;
pub mod error;
mod handlers;
pub mod state;

pub use config::Config;
pub use error::AppError;
pub use state::AppState;

use axum::{
    Json, Router, middleware,
    routing::{get, post},
};
use serde_json::json;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/api/debug/job", post(handlers::debug_create_job))
        .route("/api/debug/jobs", get(handlers::debug_list_jobs))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ))
        .with_state(state);

    Router::new()
        .route("/api/health", get(health))
        .merge(authed)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}
