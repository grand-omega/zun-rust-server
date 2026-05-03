//! Single prompt catalog. Replaces the v1 `prompts.toml` file.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{AppError, AppState};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CustomPromptRow {
    pub id: i64,
    pub label: String,
    pub description: Option<String>,
    pub text: String,
    pub workflow: String,
    pub timeout_seconds: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Deserialize)]
pub struct CreatePrompt {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub text: String,
    pub workflow: String,
    #[serde(default)]
    pub timeout_seconds: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdatePrompt {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub timeout_seconds: Option<Option<i64>>,
}

fn validate_workflow(state: &AppState, workflow: &str) -> Result<(), AppError> {
    state
        .workflows
        .supports(workflow)
        .map_err(|e| AppError::BadRequest(e.to_string()))
}

pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreatePrompt>,
) -> Result<(StatusCode, Json<CustomPromptRow>), AppError> {
    if body.label.trim().is_empty() {
        return Err(AppError::BadRequest("label must be non-empty".into()));
    }
    if body.text.trim().is_empty() {
        return Err(AppError::BadRequest("text must be non-empty".into()));
    }
    if body.text.len() > crate::MAX_PROMPT_LEN {
        return Err(AppError::BadRequest(format!(
            "text must be at most {} bytes",
            crate::MAX_PROMPT_LEN
        )));
    }
    validate_workflow(&state, &body.workflow)?;

    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "INSERT INTO custom_prompts \
         (label, description, text, workflow, timeout_seconds, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&body.label)
    .bind(&body.description)
    .bind(&body.text)
    .bind(&body.workflow)
    .bind(body.timeout_seconds)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;
    let id = res.last_insert_rowid();

    let row = fetch(&state, id).await?.ok_or(AppError::NotFound)?;
    tracing::info!(target: "audit", event = "prompt.created", id);
    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn list(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let rows: Vec<CustomPromptRow> = sqlx::query_as(
        "SELECT id, label, description, text, workflow, timeout_seconds, created_at, updated_at \
         FROM custom_prompts WHERE deleted_at IS NULL \
         ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "items": rows })))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<CustomPromptRow>, AppError> {
    let row = fetch(&state, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(row))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdatePrompt>,
) -> Result<Json<CustomPromptRow>, AppError> {
    let mut current = fetch(&state, id).await?.ok_or(AppError::NotFound)?;

    if let Some(label) = body.label {
        if label.trim().is_empty() {
            return Err(AppError::BadRequest("label must be non-empty".into()));
        }
        current.label = label;
    }
    if let Some(desc) = body.description {
        current.description = desc;
    }
    if let Some(text) = body.text {
        if text.trim().is_empty() {
            return Err(AppError::BadRequest("text must be non-empty".into()));
        }
        if text.len() > crate::MAX_PROMPT_LEN {
            return Err(AppError::BadRequest(format!(
                "text must be at most {} bytes",
                crate::MAX_PROMPT_LEN
            )));
        }
        current.text = text;
    }
    if let Some(wf) = body.workflow {
        validate_workflow(&state, &wf)?;
        current.workflow = wf;
    }
    if let Some(t) = body.timeout_seconds {
        current.timeout_seconds = t;
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE custom_prompts SET label = ?, description = ?, text = ?, \
         workflow = ?, timeout_seconds = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&current.label)
    .bind(&current.description)
    .bind(&current.text)
    .bind(&current.workflow)
    .bind(current.timeout_seconds)
    .bind(now)
    .bind(id)
    .execute(&state.db)
    .await?;

    let row = fetch(&state, id).await?.ok_or(AppError::NotFound)?;
    tracing::info!(target: "audit", event = "prompt.updated", id);
    Ok(Json(row))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE custom_prompts SET deleted_at = ? \
         WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(now)
    .bind(id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tracing::info!(target: "audit", event = "prompt.deleted", id);
    Ok(StatusCode::NO_CONTENT)
}

async fn fetch(state: &AppState, id: i64) -> Result<Option<CustomPromptRow>, AppError> {
    let row: Option<CustomPromptRow> = sqlx::query_as(
        "SELECT id, label, description, text, workflow, timeout_seconds, created_at, updated_at \
         FROM custom_prompts WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row)
}
