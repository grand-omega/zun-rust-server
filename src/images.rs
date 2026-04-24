//! Handlers that serve per-job result + thumbnail + preview images. Inputs
//! are served via `inputs::get_input_file` (they live in the cache dir and
//! are addressed by input_id, not job_id).

use std::path::Path as FsPath;

use axum::{
    Extension,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use tokio_util::io::ReaderStream;

use crate::{
    AppError, AppState,
    derived_images::{self, PREVIEW_MAX_EDGE, THUMB_MAX_EDGE},
    paths::subdir,
    state::UserId,
};

const CACHE_HEADER: &str = "private, max-age=3600";

/// Serve the full-resolution output. 409 if the job hasn't finished yet.
pub async fn get_result(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, output_path FROM jobs \
         WHERE id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(&job_id)
    .bind(user.0)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    let (status, output_path) = row;
    if status != "done" {
        return Err(AppError::NotReady);
    }
    let rel = output_path.ok_or(AppError::NotReady)?;
    let abs = state.config.data_dir.join(&rel);
    serve_file_with_ct(&abs, content_type_for(&rel), req.headers()).await
}

/// 400px JPEG. Fast path: cached file. Slow path: lazy generation.
pub async fn get_thumb(
    state: State<AppState>,
    user: Extension<UserId>,
    job_id: Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    serve_derived(
        state,
        user,
        job_id,
        req,
        "thumb_path",
        subdir::THUMBS,
        THUMB_MAX_EDGE,
    )
    .await
}

/// ~1280px JPEG, sized for full-screen phone viewing. Same lazy-fallback story.
pub async fn get_preview(
    state: State<AppState>,
    user: Extension<UserId>,
    job_id: Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    serve_derived(
        state,
        user,
        job_id,
        req,
        "preview_path",
        subdir::PREVIEWS,
        PREVIEW_MAX_EDGE,
    )
    .await
}

async fn serve_derived(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
    req: Request,
    column: &'static str,
    sub: &'static str,
    max_edge: u32,
) -> Result<Response, AppError> {
    // `column` is one of two hardcoded constants — never user input.
    let sql = format!(
        "SELECT status, output_path, {column} FROM jobs \
         WHERE id = ? AND user_id = ? AND deleted_at IS NULL"
    );
    let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(&sql)
        .bind(&job_id)
        .bind(user.0)
        .fetch_optional(&state.db)
        .await?;
    let (status, output_path, derived_rel) = row.ok_or(AppError::NotFound)?;
    if status != "done" {
        return Err(AppError::NotReady);
    }

    // Fast path: pre-generated file on disk.
    if let Some(rel) = derived_rel.as_deref() {
        let abs = state.config.data_dir.join(rel);
        if tokio::fs::metadata(&abs).await.is_ok() {
            return serve_file_with_ct(&abs, "image/jpeg", req.headers()).await;
        }
    }

    // Lazy fallback: generate on demand for jobs that finished before the
    // worker started writing this rendition (or whose file got removed).
    let output_rel = output_path.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("done job {job_id} missing output_path"))
    })?;
    let output_abs = state.config.data_dir.join(&output_rel);
    let abs = derived_images::ensure_one(
        &state.db,
        &state.config.data_dir,
        user,
        &job_id,
        &output_abs,
        sub,
        max_edge,
        column,
    )
    .await
    .map_err(AppError::Internal)?;
    serve_file_with_ct(&abs, "image/jpeg", req.headers()).await
}

/// Stream a file (configurable content-type), honoring `If-None-Match`
/// when the file's (len, mtime) ETag matches. 404 if the file is missing.
pub async fn serve_file_with_ct(
    abs: &FsPath,
    content_type: &str,
    req_headers: &HeaderMap,
) -> Result<Response, AppError> {
    let meta = tokio::fs::metadata(abs)
        .await
        .map_err(|_| AppError::NotFound)?;
    let etag = etag_for(&meta);

    if let (Some(etag_val), Some(if_none)) = (etag.as_ref(), req_headers.get(header::IF_NONE_MATCH))
        && let Ok(s) = if_none.to_str()
        && s.split(',').any(|v| v.trim() == etag_val)
    {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        if let Ok(v) = HeaderValue::from_str(etag_val) {
            not_modified.headers_mut().insert(header::ETAG, v);
        }
        not_modified.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_HEADER),
        );
        return Ok(not_modified);
    }

    let file = tokio::fs::File::open(abs)
        .await
        .map_err(|_| AppError::NotFound)?;
    let len = meta.len();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut resp = Response::new(body);
    let headers = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_HEADER),
    );
    if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
        headers.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(etag_val) = etag
        && let Ok(v) = HeaderValue::from_str(&etag_val)
    {
        headers.insert(header::ETAG, v);
    }
    Ok(resp)
}

fn etag_for(meta: &std::fs::Metadata) -> Option<String> {
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("\"{}-{}\"", meta.len(), mtime_ns))
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
