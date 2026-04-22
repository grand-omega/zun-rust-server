use axum::{Json, Router, routing::get};
use serde_json::json;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn router() -> Router {
    Router::new().route("/api/health", get(health))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}
