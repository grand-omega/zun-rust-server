//! Read-only handlers for `/api/v1/inputs/...` — clients use these to
//! display the source photo associated with a job. Inputs are otherwise
//! not user-curated.

use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::json;

use crate::{AppError, AppState, images};

#[derive(sqlx::FromRow)]
struct InputRow {
    id: i64,
    sha256: String,
    path: Option<String>,
    original_name: Option<String>,
    content_type: Option<String>,
    size_bytes: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    created_at: i64,
    last_used_at: i64,
}

pub async fn get_input(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row: InputRow = sqlx::query_as(
        "SELECT id, sha256, path, original_name, content_type, size_bytes, \
         width, height, created_at, last_used_at \
         FROM inputs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(json!({
        "id": row.id,
        "sha256": row.sha256,
        "available": row.path.is_some(),
        "original_name": row.original_name,
        "content_type": row.content_type,
        "size_bytes": row.size_bytes,
        "width": row.width,
        "height": row.height,
        "created_at": row.created_at,
        "last_used_at": row.last_used_at,
    })))
}

pub async fn get_input_file(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    req: Request,
) -> Result<Response, AppError> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT path, content_type FROM inputs WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let (path, content_type) = row.ok_or(AppError::NotFound)?;
    let path = path.ok_or(AppError::NotFound)?; // purged
    let abs = state.config.data_dir.join(&path);
    let ct = content_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    images::serve_file_with_ct(&abs, ct, req.headers()).await
}

#[allow(dead_code)]
pub fn _no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}
