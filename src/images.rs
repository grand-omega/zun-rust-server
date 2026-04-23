//! Handlers that serve per-job images (input / result / thumb).
//!
//! All three go through the DB so we can (a) enforce auth via the normal
//! middleware, (b) 404 before touching the filesystem for unknown jobs,
//! and (c) 409 a result that hasn't completed yet. Thumbnails are
//! lazily generated on first request and cached to `data/thumbs/`.

use std::path::Path as FsPath;

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use tokio_util::io::ReaderStream;

use crate::{AppError, AppState};

const CACHE_HEADER: &str = "private, max-age=3600";
const THUMB_SIDE: u32 = 400;

/// Serve the original uploaded input.
pub async fn get_input(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    req: Request,
) -> Result<Response, AppError> {
    let (rel,): (String,) = sqlx::query_as("SELECT input_path FROM jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let abs = state.config.data_dir.join(&rel);
    serve_file(&abs, content_type_for(&rel), req.headers()).await
}

/// Serve the full-resolution output. 409 if the job hasn't finished yet.
pub async fn get_result(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    req: Request,
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
    serve_file(&abs, content_type_for(&rel), req.headers()).await
}

/// Serve a 400×400 max JPEG thumbnail. Generated on first request and
/// cached to `data/thumbs/{job_id}.jpg`; `thumb_path` is also persisted on
/// the row so subsequent reads skip the decode.
pub async fn get_thumb(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    req: Request,
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
        if tokio::fs::metadata(&abs).await.is_ok() {
            return serve_file(&abs, "image/jpeg", req.headers()).await;
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

    // Write to a unique temp file in the same dir, then atomic rename. This
    // is safe under concurrent requests: the worst case is that two workers
    // both generate and the second rename overwrites — both produce
    // identical content so readers never see a torn JPEG.
    let thumb_abs_final = thumb_abs.clone();
    let tmp_abs = thumb_abs.with_extension(format!("jpg.tmp-{}", uuid::Uuid::new_v4()));
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
        std::fs::write(&tmp_abs, &buf)?;
        if let Err(e) = std::fs::rename(&tmp_abs, &thumb_abs_final) {
            // Rename failed — best-effort cleanup so we don't leak the tmp.
            let _ = std::fs::remove_file(&tmp_abs);
            return Err(e.into());
        }
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

    // Freshly generated: serve in-memory and attach the same ETag we'd
    // compute on a cached hit so the next request can revalidate.
    let etag = match tokio::fs::metadata(&thumb_abs).await {
        Ok(meta) => etag_for(&meta),
        Err(_) => None,
    };
    Ok(image_response_bytes("image/jpeg", bytes, etag))
}

/// Stream a file, honoring `If-None-Match` when the file's (len, mtime)
/// ETag matches. 404 if the file is missing.
async fn serve_file(
    abs: &FsPath,
    content_type: &'static str,
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
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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

/// Compute a weak-style ETag from file metadata. Since every served image
/// is write-once (inputs written at submit, outputs written at job done,
/// thumbs written on first generate and then stable), `(len, mtime_ns)`
/// uniquely identifies a given file's contents.
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

/// In-memory byte response used only for the freshly-generated thumbnail
/// path (where we already have the bytes and want to avoid an extra read).
fn image_response_bytes(
    content_type: &'static str,
    bytes: Vec<u8>,
    etag: Option<String>,
) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_HEADER),
    );
    if let Some(etag_val) = etag
        && let Ok(v) = HeaderValue::from_str(&etag_val)
    {
        headers.insert(header::ETAG, v);
    }
    resp
}
