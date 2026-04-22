use axum::{Json, extract::State, http::StatusCode};
use serde_json::json;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn debug_create_job(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO jobs (id, status, prompt_id, input_path, created_at) \
         VALUES (?, 'queued', 'debug', 'debug-input', ?)",
    )
    .bind(&id)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(internal)?;

    Ok(Json(json!({ "id": id })))
}

pub async fn debug_list_jobs(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT id, status, prompt_id, created_at FROM jobs ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;

    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, status, prompt_id, created_at)| {
            json!({
                "id": id,
                "status": status,
                "prompt_id": prompt_id,
                "created_at": created_at,
            })
        })
        .collect();

    Ok(Json(json!(items)))
}
