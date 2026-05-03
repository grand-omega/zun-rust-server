//! Helpers that derive smaller JPEG renditions of a job's full-res output.
//!
//! Two tiers:
//! - **thumb** (400 px max edge, JPEG q85) — gallery tile, instant on any link.
//! - **preview** (1280 px max edge, JPEG q85) — full-screen viewing on phones,
//!   indistinguishable from full-res to the eye, ~5–10× smaller.
//!
//! The worker invokes `generate_for_job` once at job-done time so the phone
//! can fetch them with no encode latency. The image handlers fall back to
//! lazy generation for jobs that finished before this code shipped.

use std::path::PathBuf;

use sqlx::SqlitePool;

use crate::paths::{self, subdir};

pub const THUMB_MAX_EDGE: u32 = 400;
pub const PREVIEW_MAX_EDGE: u32 = 1280;
const JPEG_QUALITY: u8 = 85;

/// Generate both renditions for a job whose output is at `output_abs`. Writes
/// `thumb_path` and `preview_path` columns on the row. Errors are logged but
/// not returned — failing to render a thumb must not fail the job.
pub async fn generate_for_job(
    db: &SqlitePool,
    data_dir: &std::path::Path,
    job_id: &str,
    output_abs: &std::path::Path,
) {
    if let Err(e) = render_and_persist(
        db,
        data_dir,
        job_id,
        output_abs,
        subdir::THUMBS,
        THUMB_MAX_EDGE,
        "thumb_path",
    )
    .await
    {
        tracing::warn!(job_id, error = %e, "thumb pre-generation failed (non-fatal)");
    }
    if let Err(e) = render_and_persist(
        db,
        data_dir,
        job_id,
        output_abs,
        subdir::PREVIEWS,
        PREVIEW_MAX_EDGE,
        "preview_path",
    )
    .await
    {
        tracing::warn!(job_id, error = %e, "preview pre-generation failed (non-fatal)");
    }
}

/// Lazy-generate a single rendition. Returns the absolute path of the file.
/// Used by the image handlers when the pre-generated file is missing (e.g.
/// jobs that finished before the worker started writing previews).
#[allow(clippy::too_many_arguments)]
pub async fn ensure_one(
    db: &SqlitePool,
    data_dir: &std::path::Path,
    job_id: &str,
    output_abs: &std::path::Path,
    sub: &'static str,
    max_edge: u32,
    column: &'static str,
) -> anyhow::Result<PathBuf> {
    render_and_persist(db, data_dir, job_id, output_abs, sub, max_edge, column).await
}

#[allow(clippy::too_many_arguments)]
async fn render_and_persist(
    db: &SqlitePool,
    data_dir: &std::path::Path,
    job_id: &str,
    output_abs: &std::path::Path,
    sub: &'static str,
    max_edge: u32,
    column: &'static str,
) -> anyhow::Result<PathBuf> {
    let filename = format!("{job_id}.jpg");
    let abs = paths::data_path(data_dir, sub, &filename)?;
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let abs_for_blocking = abs.clone();
    let output_for_blocking = output_abs.to_path_buf();
    let tmp = abs.with_extension(format!("jpg.tmp-{}", uuid::Uuid::new_v4()));

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let img = image::ImageReader::open(&output_for_blocking)?.decode()?;
        let resized = img.resize(max_edge, max_edge, image::imageops::FilterType::Lanczos3);
        let rgb = resized.to_rgb8();
        let mut buf: Vec<u8> = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
        rgb.write_with_encoder(encoder)?;
        std::fs::write(&tmp, &buf)?;
        // Atomic rename so concurrent readers never see a torn file.
        if let Err(e) = std::fs::rename(&tmp, &abs_for_blocking) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("derived-image join: {e}"))??;

    let rel = abs
        .strip_prefix(data_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs.to_string_lossy().into_owned());

    // Build the UPDATE dynamically. `column` is a hardcoded constant in
    // every call site — never user input — so string-substitution is safe.
    let sql = format!("UPDATE jobs SET {column} = ? WHERE id = ?");
    sqlx::query(&sql)
        .bind(&rel)
        .bind(job_id)
        .execute(db)
        .await?;
    Ok(abs)
}
