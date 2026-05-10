//! Handlers that serve per-job result + thumbnail + preview images. Inputs
//! are served via `inputs::get_input_file` (they live in the cache dir and
//! are addressed by input_id, not job_id).

use std::path::Path as FsPath;

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use tokio_util::io::ReaderStream;

use crate::{
    AppError, AppState,
    derived_images::{self, DerivedFormat, PREVIEW_MAX_EDGE, THUMB_MAX_EDGE},
    paths::subdir,
};

const CACHE_HEADER: &str = "private, max-age=3600";

/// Serve the full-resolution output. 409 if the job hasn't finished yet.
pub async fn get_result(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, output_path FROM jobs \
         WHERE id = ? AND deleted_at IS NULL",
    )
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
    serve_file_with_ct(&abs, content_type_for(&rel), req.headers()).await
}

/// 400px derived image. Fast path: cached file. Slow path: lazy generation.
pub async fn get_thumb(
    state: State<AppState>,
    job_id: Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    serve_derived(
        state,
        job_id,
        req,
        "thumb_path",
        subdir::THUMBS,
        THUMB_MAX_EDGE,
    )
    .await
}

/// ~1280px derived image, sized for full-screen phone viewing.
pub async fn get_preview(
    state: State<AppState>,
    job_id: Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    serve_derived(
        state,
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
    Path(job_id): Path<String>,
    req: Request,
    column: &'static str,
    sub: &'static str,
    max_edge: u32,
) -> Result<Response, AppError> {
    // `column` is one of two hardcoded constants — never user input.
    let sql = format!(
        "SELECT status, output_path, {column} FROM jobs \
         WHERE id = ? AND deleted_at IS NULL"
    );
    let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await?;
    let (status, output_path, mut derived_rel) = row.ok_or(AppError::NotFound)?;
    if status != "done" {
        return Err(AppError::NotReady);
    }
    let negotiated = negotiate_derived_format(req.headers());

    // Fast path: pre-generated file on disk.
    if let Some(abs) =
        cached_derived_path(&state.config.data_dir, derived_rel.as_deref(), negotiated)
        && tokio::fs::metadata(&abs).await.is_ok()
    {
        return serve_negotiated_file(&abs, negotiated, req.headers()).await;
    }
    if negotiated == DerivedFormat::Avif {
        let abs = derived_cache_path(&state.config.data_dir, sub, &job_id, negotiated)?;
        if tokio::fs::metadata(&abs).await.is_ok() {
            return serve_negotiated_file(&abs, negotiated, req.headers()).await;
        }
    }

    // Lazy fallback: generate on demand for jobs that finished before the
    // worker started writing this rendition (or whose file got removed).
    let output_rel = output_path.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("done job {job_id} missing output_path"))
    })?;
    let output_abs = state.config.data_dir.join(&output_rel);
    if negotiated == DerivedFormat::Avif
        && cached_derived_path(
            &state.config.data_dir,
            derived_rel.as_deref(),
            DerivedFormat::Jpeg,
        )
        .is_none_or(|abs| !abs.exists())
    {
        let jpeg_abs = derived_images::ensure_one(
            &state.db,
            &state.config.data_dir,
            &job_id,
            &output_abs,
            sub,
            max_edge,
            DerivedFormat::Jpeg,
            column,
        )
        .await
        .map_err(AppError::Internal)?;
        derived_rel = Some(
            jpeg_abs
                .strip_prefix(&state.config.data_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| jpeg_abs.to_string_lossy().into_owned()),
        );
    }
    let generated = derived_images::ensure_one(
        &state.db,
        &state.config.data_dir,
        &job_id,
        &output_abs,
        sub,
        max_edge,
        negotiated,
        column,
    )
    .await;
    let abs = match generated {
        Ok(abs) => abs,
        Err(e) if negotiated == DerivedFormat::Avif => {
            tracing::warn!(job_id, error = %e, "avif lazy generation failed; falling back to jpeg");
            let Some(abs) = cached_derived_path(
                &state.config.data_dir,
                derived_rel.as_deref(),
                DerivedFormat::Jpeg,
            ) else {
                return Err(AppError::Internal(e));
            };
            if tokio::fs::metadata(&abs).await.is_err() {
                return Err(AppError::Internal(e));
            }
            return serve_negotiated_file(&abs, DerivedFormat::Jpeg, req.headers()).await;
        }
        Err(e) => return Err(AppError::Internal(e)),
    };
    serve_negotiated_file(&abs, negotiated, req.headers()).await
}

fn negotiate_derived_format(headers: &HeaderMap) -> DerivedFormat {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return DerivedFormat::Jpeg;
    };
    if accept.split(',').any(|part| {
        let part = part.trim().to_ascii_lowercase();
        let mut sections = part.split(';').map(str::trim);
        let media = sections.next().unwrap_or_default();
        let allows = sections.all(|section| {
            !section.starts_with("q=")
                || section
                    .strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q > 0.0)
        });
        allows && media == "image/avif"
    }) {
        DerivedFormat::Avif
    } else {
        DerivedFormat::Jpeg
    }
}

fn cached_derived_path(
    data_dir: &FsPath,
    jpeg_rel: Option<&str>,
    format: DerivedFormat,
) -> Option<std::path::PathBuf> {
    let rel = jpeg_rel?;
    let abs = data_dir.join(rel);
    match format {
        DerivedFormat::Jpeg => Some(abs),
        DerivedFormat::Avif => Some(abs.with_extension(format.extension())),
    }
}

fn derived_cache_path(
    data_dir: &FsPath,
    sub: &'static str,
    job_id: &str,
    format: DerivedFormat,
) -> Result<std::path::PathBuf, AppError> {
    let filename = format!("{}.{}", job_id, format.extension());
    crate::paths::data_path(data_dir, sub, &filename).map_err(AppError::Internal)
}

async fn serve_negotiated_file(
    abs: &FsPath,
    format: DerivedFormat,
    req_headers: &HeaderMap,
) -> Result<Response, AppError> {
    let mut resp = serve_file_with_ct(abs, format.content_type(), req_headers).await?;
    resp.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    Ok(resp)
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
