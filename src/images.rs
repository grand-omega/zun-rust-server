//! Handlers that serve per-job images (input / result / thumb).
//!
//! All three go through the DB so we can (a) enforce auth via the normal
//! middleware, (b) 404 before touching the filesystem for unknown jobs,
//! and (c) 409 a result that hasn't completed yet. Thumbnails are
//! lazily generated on first request and cached to `data/thumbs/`.

use std::path::Path as FsPath;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{AppError, AppState};

const CACHE_HEADER: &str = "private, max-age=3600";
const THUMB_SIDE: u32 = 400;

/// Serve the original uploaded input.
pub async fn get_input(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Response, AppError> {
    let (rel,): (String,) = sqlx::query_as("SELECT input_path FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let abs = state.config.data_dir.join(&rel);
    let bytes = tokio::fs::read(&abs)
        .await
        .map_err(|_| AppError::NotFound)?;
    Ok(image_response(content_type_for(&rel), bytes))
}

/// Serve the full-resolution output. 409 if the job hasn't finished yet.
pub async fn get_result(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Response, AppError> {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status, output_path FROM jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
    let (status, output_path) = row;
    if status != "done" {
        return Err(AppError::NotReady);
    }
    let rel = output_path.ok_or(AppError::NotReady)?;
    let abs = state.config.data_dir.join(&rel);
    let bytes = tokio::fs::read(&abs)
        .await
        .map_err(|_| AppError::NotFound)?;
    Ok(image_response(content_type_for(&rel), bytes))
}

/// Serve a 400×400 max JPEG thumbnail. Generated on first request and
/// cached to `data/thumbs/{job_id}.jpg`; `thumb_path` is also persisted on
/// the row so subsequent reads skip the decode.
pub async fn get_thumb(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Response, AppError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        status: String,
        output_path: Option<String>,
        thumb_path: Option<String>,
    }
    let row: Row = sqlx::query_as("SELECT status, output_path, thumb_path FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    if row.status != "done" {
        return Err(AppError::NotReady);
    }

    // Fast path: cached thumb on disk.
    if let Some(rel) = row.thumb_path.as_deref() {
        let abs = state.config.data_dir.join(rel);
        if let Ok(bytes) = tokio::fs::read(&abs).await {
            return Ok(image_response("image/jpeg", bytes));
        }
        // Fall through to regenerate if the file is missing.
    }

    // Lazy generate.
    let output_rel = row.output_path.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("done job {job_id} missing output_path"))
    })?;
    let output_abs = state.config.data_dir.join(&output_rel);
    let thumb_rel = format!("thumbs/{job_id}.jpg");
    let thumb_abs = state.config.data_dir.join(&thumb_rel);

    if let Some(parent) = thumb_abs.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let thumb_path_for_task = thumb_abs.clone();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let img = image::ImageReader::open(&output_abs)?.decode()?;
        let thumb = img.resize(
            THUMB_SIDE,
            THUMB_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
        let mut buf: Vec<u8> = Vec::new();
        thumb.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )?;
        std::fs::write(&thumb_path_for_task, &buf)?;
        Ok(buf)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("thumb join: {e}")))?
    .map_err(AppError::Internal)?;

    // Remember where we wrote it.
    sqlx::query("UPDATE jobs SET thumb_path = ? WHERE id = ?")
        .bind(&thumb_rel)
        .bind(&job_id)
        .execute(&state.db)
        .await?;

    Ok(image_response("image/jpeg", bytes))
}

fn content_type_for(rel: &str) -> &'static str {
    match FsPath::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

fn image_response(content_type: &'static str, bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(CACHE_HEADER),
            ),
        ],
        Body::from(bytes),
    )
        .into_response()
}
