//! Request handlers for `/api/v1/jobs/...`. All handlers are user-scoped:
//! every SQL touching user-owned rows takes `UserId` as a mandatory filter.

use axum::{
    Extension, Json,
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    AppError, AppState,
    hash::{is_valid_sha256_hex, sha256_hex},
    paths::{self, subdir},
    state::UserId,
};

/// Per-job random seed. ComfyUI's sampler validators reject negative seeds,
/// so we mask off the sign bit to keep the value in `[0, i64::MAX]` — still
/// 63 bits of entropy, fits in SQLite's INTEGER, and ComfyUI accepts it.
fn random_seed() -> i64 {
    (rand::random::<u64>() >> 1) as i64
}

// --------------------------- POST /api/v1/jobs ---------------------------

/// JSON variant of submit. Used when the client already knows the input
/// hash is in cache; no bytes attached.
#[derive(Deserialize)]
struct SubmitJson {
    input_sha256: String,
    #[serde(default)]
    input_name: Option<String>,
    #[serde(default)]
    prompt_id: Option<i64>,
    #[serde(default)]
    prompt_text: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
}

/// Resolved submit fields (multipart fields collected, or JSON parsed).
struct SubmitFields {
    input_sha256: String,
    input_name: Option<String>,
    prompt_id: Option<i64>,
    prompt_text: Option<String>,
    workflow_override: Option<String>,
    /// Raw bytes + ext for multipart submits. None for JSON submits.
    upload: Option<Upload>,
}

struct Upload {
    bytes: Vec<u8>,
    ext: &'static str,
    content_type: &'static str,
}

pub async fn submit_job(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    req: Request,
) -> Result<(StatusCode, HeaderMap, Json<serde_json::Value>), AppError> {
    let fields = parse_submit(&state, req).await?;

    if !is_valid_sha256_hex(&fields.input_sha256) {
        return Err(AppError::BadRequest(
            "input_sha256 must be 64 lowercase hex chars".into(),
        ));
    }

    let (resolved_workflow, resolved_prompt_text, resolved_prompt_id, prompt_timeout_seconds) =
        resolve_prompt(&state, user, &fields).await?;

    let input_id = resolve_input(&state, user, &fields).await?;

    let job_id = uuid::Uuid::new_v4().to_string();
    let seed = random_seed();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO jobs \
         (id, user_id, input_id, prompt_id, prompt_text, workflow, seed, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?)",
    )
    .bind(&job_id)
    .bind(user.0)
    .bind(input_id)
    .bind(resolved_prompt_id)
    .bind(&resolved_prompt_text)
    .bind(&resolved_workflow)
    .bind(seed)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Wake the worker (full channel = wake already pending).
    let _ = state.worker_tx.try_send(());

    tracing::info!(
        target: "audit",
        event = "job.submitted",
        user_id = user.0,
        %job_id,
        input_id,
        prompt_id = ?resolved_prompt_id,
        workflow = %resolved_workflow,
        seed,
        timeout_s = prompt_timeout_seconds,
    );

    let mut headers = HeaderMap::new();
    if let Ok(loc) = HeaderValue::from_str(&format!("/api/v1/jobs/{job_id}")) {
        headers.insert(header::LOCATION, loc);
    }
    Ok((
        StatusCode::ACCEPTED,
        headers,
        Json(json!({ "job_id": job_id, "input_id": input_id })),
    ))
}

async fn parse_submit(state: &AppState, req: Request) -> Result<SubmitFields, AppError> {
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if ct.starts_with("multipart/form-data") {
        parse_multipart(state, req).await
    } else if ct.starts_with("application/json") {
        let Json(body): Json<SubmitJson> = Json::from_request(req, state)
            .await
            .map_err(|e| AppError::BadRequest(format!("invalid json: {e}")))?;
        Ok(SubmitFields {
            input_sha256: body.input_sha256,
            input_name: body.input_name,
            prompt_id: body.prompt_id,
            prompt_text: body.prompt_text,
            workflow_override: body.workflow,
            upload: None,
        })
    } else {
        Err(AppError::BadRequest(format!(
            "unsupported content-type {ct:?}; expected multipart/form-data or application/json"
        )))
    }
}

async fn parse_multipart(state: &AppState, req: Request) -> Result<SubmitFields, AppError> {
    let mut mp = Multipart::from_request(req, state)
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart: {e}")))?;

    let mut input_sha256: Option<String> = None;
    let mut input_name: Option<String> = None;
    let mut prompt_id: Option<i64> = None;
    let mut prompt_text: Option<String> = None;
    let mut workflow: Option<String> = None;
    let mut upload: Option<Upload> = None;

    while let Some(field) = mp.next_field().await? {
        match field.name() {
            Some("image") => {
                let content_type = field.content_type().unwrap_or("").to_string();
                let (ext, ct) = match content_type.as_str() {
                    "image/jpeg" => ("jpg", "image/jpeg"),
                    "image/png" => ("png", "image/png"),
                    other => {
                        return Err(AppError::BadRequest(format!(
                            "unsupported image content-type '{other}' (expected image/jpeg or image/png)"
                        )));
                    }
                };
                let bytes = field.bytes().await?.to_vec();
                upload = Some(Upload {
                    bytes,
                    ext,
                    content_type: ct,
                });
            }
            Some("input_sha256") => input_sha256 = Some(field.text().await?),
            Some("input_name") => input_name = Some(field.text().await?),
            Some("prompt_id") => {
                let txt = field.text().await?;
                let id: i64 = txt
                    .parse()
                    .map_err(|_| AppError::BadRequest("prompt_id must be an integer".into()))?;
                prompt_id = Some(id);
            }
            Some("prompt_text") => prompt_text = Some(field.text().await?),
            Some("workflow") => workflow = Some(field.text().await?),
            _ => {}
        }
    }

    let input_sha256 =
        input_sha256.ok_or_else(|| AppError::BadRequest("input_sha256 field required".into()))?;
    Ok(SubmitFields {
        input_sha256,
        input_name,
        prompt_id,
        prompt_text,
        workflow_override: workflow,
        upload,
    })
}

/// Returns `(workflow_name, prompt_text_for_jobs_row, prompt_id_for_jobs_row, timeout_s)`.
/// Exactly one of prompt_id / prompt_text must be set; on prompt_text the
/// caller must also supply `workflow`.
async fn resolve_prompt(
    state: &AppState,
    user: UserId,
    fields: &SubmitFields,
) -> Result<(String, Option<String>, Option<i64>, u64), AppError> {
    match (fields.prompt_id, fields.prompt_text.as_deref()) {
        (Some(_), Some(_)) => Err(AppError::BadRequest(
            "supply prompt_id OR prompt_text, not both".into(),
        )),
        (None, None) => Err(AppError::BadRequest(
            "one of prompt_id or prompt_text is required".into(),
        )),
        (Some(pid), None) => {
            let row: Option<(String, String, Option<i64>)> = sqlx::query_as(
                "SELECT text, workflow, timeout_seconds FROM custom_prompts \
                 WHERE id = ? AND user_id = ? AND deleted_at IS NULL",
            )
            .bind(pid)
            .bind(user.0)
            .fetch_optional(&state.db)
            .await?;
            let (_text, workflow, timeout) =
                row.ok_or_else(|| AppError::BadRequest(format!("unknown prompt_id: {pid}")))?;
            state
                .workflows
                .supports(&workflow)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
            // jobs.prompt_text stays null when prompt_id is set; the worker
            // dereferences `text` from the prompt row at job-run time.
            Ok((
                workflow,
                None,
                Some(pid),
                timeout
                    .map(|t| t as u64)
                    .unwrap_or(crate::DEFAULT_TIMEOUT_SECONDS),
            ))
        }
        (None, Some(text)) => {
            let workflow = fields.workflow_override.as_deref().ok_or_else(|| {
                AppError::BadRequest("workflow field required when prompt_text is set".into())
            })?;
            state
                .workflows
                .supports(workflow)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
            if text.trim().is_empty() {
                return Err(AppError::BadRequest("prompt_text must be non-empty".into()));
            }
            if text.len() > crate::MAX_PROMPT_LEN {
                return Err(AppError::BadRequest(format!(
                    "prompt_text must be at most {} bytes",
                    crate::MAX_PROMPT_LEN
                )));
            }
            Ok((
                workflow.to_string(),
                Some(text.to_string()),
                None,
                crate::DEFAULT_TIMEOUT_SECONDS,
            ))
        }
    }
}

/// Cache-or-upload flow for the input. Returns the resolved input_id.
async fn resolve_input(
    state: &AppState,
    user: UserId,
    fields: &SubmitFields,
) -> Result<i64, AppError> {
    let now = chrono::Utc::now().timestamp();

    // Look for an existing row for this (user, sha).
    let mut existing: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT id, path FROM inputs WHERE user_id = ? AND sha256 = ?")
            .bind(user.0)
            .bind(&fields.input_sha256)
            .fetch_optional(&state.db)
            .await?;

    // If we have a row that claims a path, verify the file is actually on
    // disk. It might be gone for reasons outside our control (manual cleanup,
    // partial restore, disk corruption). If missing, demote the row to
    // NULL-path so the rest of this fn treats it as "needs upload".
    if let Some((id, Some(path))) = existing.as_ref() {
        let id_v = *id;
        let abs = state.config.data_dir.join(path);
        if tokio::fs::metadata(&abs).await.is_ok() {
            sqlx::query("UPDATE inputs SET last_used_at = ? WHERE id = ? AND user_id = ?")
                .bind(now)
                .bind(id_v)
                .bind(user.0)
                .execute(&state.db)
                .await?;
            return Ok(id_v);
        }
        tracing::warn!(
            input_id = id_v,
            path = %abs.display(),
            "cached input file missing on disk; clearing path",
        );
        sqlx::query("UPDATE inputs SET path = NULL WHERE id = ? AND user_id = ?")
            .bind(id_v)
            .bind(user.0)
            .execute(&state.db)
            .await?;
        existing = Some((id_v, None));
    }

    match (existing, fields.upload.as_ref()) {
        // Handled above.
        (Some((_, Some(_))), _) => unreachable!(),
        // Hash-only request, no row OR row with NULL path → caller must re-upload.
        (existing_row, None) => Err(AppError::NeedUpload {
            input_id: existing_row.map(|(id, _)| id),
        }),
        // Multipart with bytes; either no row, or row with NULL path → write.
        (existing_row, Some(upload)) => {
            // Verify the bytes match the claimed hash. Cheap insurance —
            // dedup correctness depends on it.
            let actual = sha256_hex(&upload.bytes);
            if actual != fields.input_sha256 {
                return Err(AppError::BadRequest(format!(
                    "image bytes hash {actual} does not match input_sha256 {}",
                    fields.input_sha256
                )));
            }
            // Write the file under <cache>/<sha256>.<ext>.
            let filename = format!("{}.{}", fields.input_sha256, upload.ext);
            let abs = paths::user_data_path(
                &state.config.data_dir,
                user,
                subdir::CACHE_INPUTS,
                &filename,
            )?;
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            paths::atomic_write(&abs, &upload.bytes).await?;

            // Read dimensions best-effort; non-fatal.
            let bytes_for_dim = upload.bytes.clone();
            let (w, h) = match tokio::task::spawn_blocking(move || {
                image::ImageReader::new(std::io::Cursor::new(&bytes_for_dim))
                    .with_guessed_format()
                    .ok()
                    .and_then(|r| r.into_dimensions().ok())
            })
            .await
            {
                Ok(Some((w, h))) => (Some(w as i64), Some(h as i64)),
                _ => (None, None),
            };

            let rel = relative_for_db(&abs, &state.config.data_dir);
            let size = upload.bytes.len() as i64;

            let id: i64 = if let Some((id, _)) = existing_row {
                sqlx::query(
                    "UPDATE inputs SET path = ?, original_name = ?, content_type = ?, \
                     size_bytes = ?, width = ?, height = ?, last_used_at = ? \
                     WHERE id = ? AND user_id = ?",
                )
                .bind(&rel)
                .bind(&fields.input_name)
                .bind(upload.content_type)
                .bind(size)
                .bind(w)
                .bind(h)
                .bind(now)
                .bind(id)
                .bind(user.0)
                .execute(&state.db)
                .await?;
                id
            } else {
                let res = sqlx::query(
                    "INSERT INTO inputs \
                     (user_id, sha256, path, original_name, content_type, size_bytes, width, height, created_at, last_used_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(user.0)
                .bind(&fields.input_sha256)
                .bind(&rel)
                .bind(&fields.input_name)
                .bind(upload.content_type)
                .bind(size)
                .bind(w)
                .bind(h)
                .bind(now)
                .bind(now)
                .execute(&state.db)
                .await?;
                res.last_insert_rowid()
            };
            Ok(id)
        }
    }
}

fn relative_for_db(abs: &std::path::Path, data_dir: &std::path::Path) -> String {
    abs.strip_prefix(data_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs.to_string_lossy().into_owned())
}

// --------------------------- GET /api/v1/jobs ----------------------------

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub status: Option<String>,
    pub input_id: Option<i64>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    created_at: i64,
    id: String,
}

fn encode_cursor(c: &Cursor) -> String {
    let json = serde_json::to_vec(c).expect("cursor encodes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_cursor(s: &str) -> Result<Cursor, AppError> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    serde_json::from_slice(&raw).map_err(|_| AppError::BadRequest("invalid cursor".into()))
}

#[derive(sqlx::FromRow)]
struct JobSummaryRow {
    id: String,
    input_id: i64,
    prompt_id: Option<i64>,
    prompt_text: Option<String>,
    workflow: String,
    seed: i64,
    status: String,
    created_at: i64,
    completed_at: Option<i64>,
}

pub async fn list_jobs(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    // Build dynamically based on which filters are present. SQLite/sqlx
    // doesn't have a great parameter-list builder; the chained query_as
    // would need 8 variants, so use a single QueryBuilder.
    let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, input_id, prompt_id, prompt_text, workflow, seed, status, created_at, completed_at \
         FROM jobs WHERE user_id = ",
    );
    qb.push_bind(user.0);
    qb.push(" AND deleted_at IS NULL");

    if let Some(s) = q.status.as_deref() {
        if !matches!(s, "queued" | "running" | "done" | "failed" | "cancelled") {
            return Err(AppError::BadRequest(format!("unknown status: {s}")));
        }
        qb.push(" AND status = ");
        qb.push_bind(s.to_string());
    }
    if let Some(input_id) = q.input_id {
        qb.push(" AND input_id = ");
        qb.push_bind(input_id);
    }
    if let Some(c) = q.cursor.as_deref() {
        let cur = decode_cursor(c)?;
        // (created_at, id) < (?, ?) keyset pagination — stable under inserts.
        qb.push(" AND (created_at < ");
        qb.push_bind(cur.created_at);
        qb.push(" OR (created_at = ");
        qb.push_bind(cur.created_at);
        qb.push(" AND id < ");
        qb.push_bind(cur.id);
        qb.push("))");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
    qb.push_bind(limit);

    let rows: Vec<JobSummaryRow> = qb.build_query_as().fetch_all(&state.db).await?;

    let next_cursor = if rows.len() as i64 == limit {
        rows.last().map(|r| {
            encode_cursor(&Cursor {
                created_at: r.created_at,
                id: r.id.clone(),
            })
        })
    } else {
        None
    };

    let items: Vec<_> = rows
        .into_iter()
        .map(|r| {
            let duration_seconds = r.completed_at.map(|c| c - r.created_at);
            json!({
                "id": r.id,
                "input_id": r.input_id,
                "prompt_id": r.prompt_id,
                "prompt_text": r.prompt_text,
                "workflow": r.workflow,
                "seed": r.seed,
                "status": r.status,
                "created_at": r.created_at,
                "completed_at": r.completed_at,
                "duration_seconds": duration_seconds,
            })
        })
        .collect();

    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })).into_response())
}

// ---------------------- GET /api/v1/jobs/{id} ----------------------------

#[derive(sqlx::FromRow)]
struct JobFullRow {
    id: String,
    input_id: i64,
    prompt_id: Option<i64>,
    prompt_text: Option<String>,
    workflow: String,
    seed: i64,
    status: String,
    error_message: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    output_path: Option<String>,
}

pub async fn get_job(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row: JobFullRow = sqlx::query_as(
        "SELECT id, input_id, prompt_id, prompt_text, workflow, seed, status, \
         error_message, created_at, started_at, completed_at, width, height, output_path \
         FROM jobs WHERE id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(&job_id)
    .bind(user.0)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let sidecar_metadata = read_output_sidecar_metadata(&state, &row).await;

    Ok(Json(json!({
        "id": row.id,
        "input_id": row.input_id,
        "prompt_id": row.prompt_id,
        "prompt_text": row.prompt_text,
        "workflow": row.workflow,
        "seed": row.seed,
        "status": row.status,
        "error": row.error_message,
        "created_at": row.created_at,
        "started_at": row.started_at,
        "completed_at": row.completed_at,
        "width": row.width,
        "height": row.height,
        "metadata": sidecar_metadata,
    })))
}

async fn read_output_sidecar_metadata(
    state: &AppState,
    row: &JobFullRow,
) -> Option<serde_json::Value> {
    let output_path = row.output_path.as_ref()?;
    let sidecar = state
        .config
        .data_dir
        .join(output_path)
        .with_extension("json");
    let raw = tokio::fs::read(&sidecar).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

// ---------- DELETE /api/v1/jobs/{id} (soft) + restore --------------------

pub async fn delete_job(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE jobs SET deleted_at = ? \
         WHERE id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(now)
    .bind(&job_id)
    .bind(user.0)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tracing::info!(target: "audit", event = "job.deleted", user_id = user.0, %job_id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn cancel_job(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let now = chrono::Utc::now().timestamp();
    // Atomically transition queued→cancelled or running→cancelled so the
    // worker can't race past us and mark the row done/failed.
    let res = sqlx::query(
        "UPDATE jobs SET status = 'cancelled', completed_at = ? \
         WHERE id = ? AND user_id = ? AND status IN ('queued', 'running')",
    )
    .bind(now)
    .bind(&job_id)
    .bind(user.0)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    // Best-effort interrupt of ComfyUI. If the job was queued and never made
    // it to the GPU, the interrupt is a harmless no-op. If it was running,
    // this stops the GPU work immediately. Either way we've already
    // transitioned the row, so the worker's downstream mark_failed (which is
    // gated on status='running') will be a no-op too.
    if let Err(e) = state.comfy.interrupt().await {
        tracing::warn!(%job_id, error = %e, "comfy /interrupt failed; row already cancelled");
    }
    tracing::info!(target: "audit", event = "job.cancelled", user_id = user.0, %job_id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn restore_job(
    State(state): State<AppState>,
    Extension(user): Extension<UserId>,
    Path(job_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let res = sqlx::query(
        "UPDATE jobs SET deleted_at = NULL \
         WHERE id = ? AND user_id = ? AND deleted_at IS NOT NULL",
    )
    .bind(&job_id)
    .bind(user.0)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tracing::info!(target: "audit", event = "job.restored", user_id = user.0, %job_id);
    Ok(StatusCode::NO_CONTENT)
}
