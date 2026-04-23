use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;

use crate::{AppError, AppState, prompts::PromptDto};

// ---------- debug (M2) ----------

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

// ---------- prompts (M4) ----------

pub async fn list_prompts(State(state): State<AppState>) -> Json<Vec<PromptDto>> {
    let mut items: Vec<PromptDto> = state.prompts.values().map(PromptDto::from).collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Json(items)
}

// ---------- jobs (M4) ----------

pub async fn submit_job(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let mut image_bytes: Option<Vec<u8>> = None;
    let mut image_ext: Option<&'static str> = None;
    let mut prompt_id: Option<String> = None;

    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("image") => {
                let content_type = field.content_type().unwrap_or("").to_string();
                image_ext = Some(match content_type.as_str() {
                    "image/jpeg" => "jpg",
                    "image/png" => "png",
                    other => {
                        return Err(AppError::BadRequest(format!(
                            "unsupported image content-type: {other}"
                        )));
                    }
                });
                image_bytes = Some(field.bytes().await?.to_vec());
            }
            Some("prompt_id") => {
                prompt_id = Some(field.text().await?);
            }
            _ => { /* ignore unknown fields */ }
        }
    }

    let image_bytes =
        image_bytes.ok_or_else(|| AppError::BadRequest("image field is required".into()))?;
    let image_ext =
        image_ext.ok_or_else(|| AppError::BadRequest("image field is required".into()))?;
    let prompt_id =
        prompt_id.ok_or_else(|| AppError::BadRequest("prompt_id field is required".into()))?;

    if !state.prompts.contains_key(&prompt_id) {
        return Err(AppError::UnknownPrompt(prompt_id));
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    let rel_input = format!("inputs/{job_id}.{image_ext}");
    let abs_input = state.config.data_dir.join(&rel_input);
    if let Some(parent) = abs_input.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&abs_input, &image_bytes).await?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO jobs (id, status, prompt_id, input_path, created_at) \
         VALUES (?, 'queued', ?, ?, ?)",
    )
    .bind(&job_id)
    .bind(&prompt_id)
    .bind(&rel_input)
    .bind(now)
    .execute(&state.db)
    .await?;

    tracing::info!(%job_id, %prompt_id, "job submitted");

    Ok((StatusCode::CREATED, Json(json!({ "job_id": job_id }))))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub before: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct JobSummaryRow {
    id: String,
    prompt_id: String,
    created_at: i64,
    completed_at: Option<i64>,
}

pub async fn list_jobs(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let status = q.status.as_deref().unwrap_or("done");
    let limit = q.limit.unwrap_or(30).clamp(1, 100);

    let rows: Vec<JobSummaryRow> = if let Some(before) = q.before {
        sqlx::query_as(
            "SELECT id, prompt_id, created_at, completed_at FROM jobs \
             WHERE status = ? AND created_at < ? \
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(status)
        .bind(before)
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, prompt_id, created_at, completed_at FROM jobs \
             WHERE status = ? \
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    };

    let items: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let duration_seconds = row.completed_at.map(|completed| completed - row.created_at);
            let prompt_label = state.prompts.get(&row.prompt_id).map(|p| p.label.clone());
            json!({
                "id": row.id,
                "prompt_id": row.prompt_id,
                "prompt_label": prompt_label,
                "created_at": row.created_at,
                "duration_seconds": duration_seconds,
            })
        })
        .collect();

    Ok(Json(json!(items)))
}

#[derive(sqlx::FromRow)]
struct JobFilesRow {
    input_path: String,
    output_path: Option<String>,
    thumb_path: Option<String>,
}

pub async fn delete_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let row: JobFilesRow =
        sqlx::query_as("SELECT input_path, output_path, thumb_path FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;

    let paths: Vec<String> = std::iter::once(row.input_path)
        .chain(row.output_path)
        .chain(row.thumb_path)
        .collect();

    for rel in paths {
        let abs = state.config.data_dir.join(&rel);
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    path = %abs.display(),
                    error = %e,
                    job_id = %job_id,
                    "failed to remove file during job delete"
                );
            }
        }
    }

    sqlx::query("DELETE FROM jobs WHERE id = ?")
        .bind(&job_id)
        .execute(&state.db)
        .await?;

    tracing::info!(%job_id, "job deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(sqlx::FromRow)]
struct JobStatusRow {
    id: String,
    status: String,
    prompt_id: String,
    error_message: Option<String>,
    created_at: i64,
    completed_at: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
}

pub async fn get_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row: JobStatusRow = sqlx::query_as(
        "SELECT id, status, prompt_id, error_message, created_at, completed_at, width, height \
         FROM jobs WHERE id = ?",
    )
    .bind(&job_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let prompt_label = state.prompts.get(&row.prompt_id).map(|p| p.label.clone());

    Ok(Json(json!({
        "id": row.id,
        "status": row.status,
        "prompt_id": row.prompt_id,
        "prompt_label": prompt_label,
        "progress": serde_json::Value::Null,
        "error": row.error_message,
        "created_at": row.created_at,
        "completed_at": row.completed_at,
        "width": row.width,
        "height": row.height,
    })))
}
