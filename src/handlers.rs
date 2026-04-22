use axum::{Json, extract::State};
use serde_json::json;

use crate::{AppError, AppState};

pub async fn debug_create_job(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO jobs (id, status, prompt_id, input_path, created_at) \
         VALUES (?, 'queued', 'debug', 'debug-input', ?)",
    )
    .bind(&id)
    .bind(now)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({ "id": id })))
}

pub async fn debug_list_jobs(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT id, status, prompt_id, created_at FROM jobs ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await?;

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
