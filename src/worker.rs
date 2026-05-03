//! Background worker that drains queued jobs through ComfyUI.
//!
//! Lifecycle per job:
//! 1. Reset `running` rows left over from a previous crash (on startup).
//! 2. Pick the oldest `queued` row.
//! 3. Mark it `running`, upload the input, open a ws tagged with a per-job
//!    `client_id`, submit the patched workflow, wait on the ws for the
//!    terminal event (bounded by a per-job timeout), fetch `/history` once
//!    for the structured outputs, download the primary output, write to
//!    disk, mark `done`.
//! 4. Any error → mark `failed` with message; move on.
//!
//! Concurrency: exactly one job at a time. FLUX2 saturates the GPU.

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::{
    process::Command,
    sync::{mpsc, watch},
};

use crate::{
    AppState, comfy,
    paths::{self, subdir},
    state::UserId,
    workflow,
};

const IDLE_TICK: Duration = Duration::from_secs(30);

/// Spawn the worker on the current tokio runtime. Returns the JoinHandle
/// mostly for completeness — in production we let it run forever.
pub fn spawn(
    state: AppState,
    wake: mpsc::Receiver<()>,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(state, wake, shutdown))
}

async fn run(state: AppState, mut wake: mpsc::Receiver<()>, mut shutdown: watch::Receiver<bool>) {
    if let Err(e) = reset_running_jobs(&state.db).await {
        tracing::error!(error = %e, "could not reset running jobs on startup");
    }

    loop {
        if *shutdown.borrow() {
            tracing::info!("worker shutting down (queue drain complete)");
            return;
        }

        loop {
            if *shutdown.borrow() {
                tracing::info!("worker shutting down (mid-drain)");
                return;
            }
            match fetch_oldest_queued(&state.db).await {
                Ok(Some(job)) => {
                    let job_id = job.id.clone();
                    if let Err(e) = process_job(&state, &job).await {
                        tracing::error!(
                            target: "audit",
                            event = "job.failed",
                            job_id = %job_id,
                            user_id = job.user_id,
                            error = ?e,
                            "job failed",
                        );
                        if let Err(mark_err) =
                            mark_failed(&state.db, &job_id, &format!("{e:#}")).await
                        {
                            tracing::error!(job_id = %job_id, error = ?mark_err, "could not mark job failed");
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "queue fetch failed; backing off");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    break;
                }
            }
        }

        tokio::select! {
            _ = wake.recv() => {},
            _ = tokio::time::sleep(IDLE_TICK) => {},
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("worker shutting down (idle)");
                    return;
                }
            }
        }
    }
}

#[derive(sqlx::FromRow)]
struct QueuedJob {
    id: String,
    user_id: i64,
    input_id: i64,
    prompt_id: Option<i64>,
    prompt_text: Option<String>,
    workflow: String,
    seed: i64,
}

async fn fetch_oldest_queued(db: &SqlitePool) -> anyhow::Result<Option<QueuedJob>> {
    let row = sqlx::query_as::<_, QueuedJob>(
        "SELECT id, user_id, input_id, prompt_id, prompt_text, workflow, seed \
         FROM jobs WHERE status = 'queued' ORDER BY created_at ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row)
}

async fn reset_running_jobs(db: &SqlitePool) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE jobs SET status = 'queued' WHERE status = 'running'")
        .execute(db)
        .await?;
    let n = result.rows_affected();
    if n > 0 {
        tracing::warn!(n, "reset orphaned running jobs to queued on startup");
    }
    Ok(())
}

/// Atomically claim a queued job. Returns `Ok(true)` if we transitioned
/// queued→running, `Ok(false)` if the row is no longer queued (someone
/// cancelled/deleted between fetch and claim). Caller must abort processing
/// when this returns false.
async fn mark_running(db: &SqlitePool, job_id: &str) -> anyhow::Result<bool> {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE jobs SET status = 'running', started_at = ? \
         WHERE id = ? AND status = 'queued'",
    )
    .bind(now)
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() == 1)
}

async fn update_comfy_prompt_id(
    db: &SqlitePool,
    job_id: &str,
    comfy_id: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE jobs SET comfy_prompt_id = ? WHERE id = ?")
        .bind(comfy_id)
        .bind(job_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Transition running→done. Returns `Ok(false)` if the row is no longer in
/// `running` (e.g. user cancelled while ComfyUI was busy); caller should
/// then skip derived-image generation since the job is no longer "ours".
async fn mark_done(
    db: &SqlitePool,
    job_id: &str,
    output_path: &str,
    width: Option<i64>,
    height: Option<i64>,
) -> anyhow::Result<bool> {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "UPDATE jobs SET status = 'done', output_path = ?, completed_at = ?, \
         width = ?, height = ? WHERE id = ? AND status = 'running'",
    )
    .bind(output_path)
    .bind(now)
    .bind(width)
    .bind(height)
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() == 1)
}

async fn mark_failed(db: &SqlitePool, job_id: &str, error_message: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    // Gated on status='running': if the user cancelled the job concurrently,
    // the cancel handler has already flipped the row to 'cancelled' and we
    // must not overwrite that with 'failed'.
    sqlx::query(
        "UPDATE jobs SET status = 'failed', error_message = ?, completed_at = ? \
         WHERE id = ? AND status = 'running'",
    )
    .bind(error_message)
    .bind(now)
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn process_job(state: &AppState, job: &QueuedJob) -> anyhow::Result<()> {
    let started_at = std::time::Instant::now();
    let user = UserId(job.user_id);
    if !mark_running(&state.db, &job.id).await? {
        // Lost the queued→running race: the row is no longer queued (most
        // likely cancelled or hard-deleted between fetch and now). Drop the
        // job silently — there's nothing to do, and mark_failed would
        // overwrite the new status.
        tracing::info!(
            target: "audit",
            event = "job.skipped_not_queued",
            job_id = %job.id,
            user_id = job.user_id,
        );
        return Ok(());
    }
    tracing::info!(
        target: "audit",
        event = "job.running",
        job_id = %job.id,
        user_id = job.user_id,
        workflow = %job.workflow,
        seed = job.seed,
    );

    let (prompt_text, timeout_seconds) = resolve_prompt_and_timeout(state, job).await?;

    // Read input bytes from the per-user cache by input_id.
    let input_row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT path, original_name FROM inputs WHERE id = ? AND user_id = ?")
            .bind(job.input_id)
            .bind(job.user_id)
            .fetch_optional(&state.db)
            .await?;
    let (input_path, input_original_name) =
        input_row.ok_or_else(|| anyhow::anyhow!("input row {} disappeared", job.input_id))?;
    let input_rel = input_path
        .ok_or_else(|| anyhow::anyhow!("input file purged for input_id {}", job.input_id))?;
    let input_abs = state.config.data_dir.join(&input_rel);

    if job.workflow == workflow::FLUX2_KLEIN_9B_KV_EXPERIMENTAL {
        return process_flux2_klein_9b_kv_diffusers(
            state,
            job,
            user,
            &prompt_text,
            &input_abs,
            &input_rel,
            input_original_name.as_deref(),
            started_at,
        )
        .await;
    }

    // Resolve ComfyUI workflow template only after the virtual Diffusers
    // branch has had a chance to claim its model id.
    let template = state
        .workflows
        .supported_template(&job.workflow)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let input_bytes = tokio::fs::read(&input_abs)
        .await
        .map_err(|e| anyhow::anyhow!("read input {}: {e}", input_abs.display()))?;

    let ext = std::path::Path::new(&input_rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg");
    let upload_name = format!("zun_{}.{ext}", job.id);
    let stored_name = state.comfy.upload_image(input_bytes, &upload_name).await?;

    // Patch workflow: prompt + image + filename prefix + seed.
    let patched =
        workflow::build_edit_workflow(template, &prompt_text, &stored_name, &job.id, job.seed);

    // Open ws BEFORE submit so we don't miss events between queue and execute.
    let client_id = uuid::Uuid::new_v4().to_string();
    let mut ws = state.comfy.connect_ws(&client_id).await?;

    let comfy_prompt_id = state.comfy.submit_prompt(&patched, &client_id).await?;
    update_comfy_prompt_id(&state.db, &job.id, &comfy_prompt_id).await?;
    tracing::info!(job_id = %job.id, comfy_prompt_id = %comfy_prompt_id, "submitted to comfyui");

    let timeout = Duration::from_secs(timeout_seconds);
    tokio::time::timeout(timeout, comfy::await_completion(&mut ws, &comfy_prompt_id))
        .await
        .map_err(|_| anyhow::anyhow!("comfyui timeout after {timeout_seconds}s"))??;

    let entry = state
        .comfy
        .get_history(&comfy_prompt_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("comfy /history empty after completion for {comfy_prompt_id}")
        })?;

    if !entry.succeeded() {
        let status_str = entry.status.status_str.as_deref().unwrap_or("unknown");
        anyhow::bail!("comfyui execution failed (status={status_str})");
    }

    let prefix = format!("zun_{}", job.id);
    let output_img = entry
        .primary_output(&prefix)
        .ok_or_else(|| {
            anyhow::anyhow!("no output image matched prefix `{prefix}` in comfy history")
        })?
        .clone();

    let bytes = state
        .comfy
        .view(
            &output_img.filename,
            &output_img.subfolder,
            &output_img.r#type,
        )
        .await?;
    let abs_output = paths::user_data_path(
        &state.config.data_dir,
        user,
        subdir::OUTPUTS,
        &output_img.filename,
    )?;
    if let Some(parent) = abs_output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    paths::atomic_write(&abs_output, &bytes).await?;
    let rel_output = relative_for_db(&abs_output, &state.config.data_dir);

    finalize_output(
        state,
        job,
        user,
        &abs_output,
        &rel_output,
        bytes.len(),
        started_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn process_flux2_klein_9b_kv_diffusers(
    state: &AppState,
    job: &QueuedJob,
    user: UserId,
    prompt_text: &str,
    input_abs: &std::path::Path,
    input_rel: &str,
    input_original_name: Option<&str>,
    started_at: std::time::Instant,
) -> anyhow::Result<()> {
    const STEPS: u32 = 4;
    const HEIGHT: u32 = 1024;
    const WIDTH: u32 = 768;
    const PIPELINE: &str = "Flux2KleinKVPipeline";
    const DTYPE: &str = "bfloat16";
    const OFFLOAD_MODE: &str = "sequential";

    // The python runner reads the model path itself; we only record it for
    // audit and the sidecar, so it must be configured for those to be
    // truthful. Bail rather than emit lies into the audit log.
    let model_path = state.config.diffusers_model_path.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "diffusers_model_path is not set in config.toml; required for the {} workflow",
            workflow::FLUX2_KLEIN_9B_KV_EXPERIMENTAL,
        )
    })?;

    let workflows_dir = state.config.resolved_workflows_dir();
    let project_root = std::fs::canonicalize(&workflows_dir)
        .map_err(|e| anyhow::anyhow!("resolve workflows dir {}: {e}", workflows_dir.display()))?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("workflows dir has no parent: {}", workflows_dir.display()))?
        .to_path_buf();
    let script_rel = "scripts/run_flux2_9b_kv_experimental.py";
    let script_abs = project_root.join(script_rel);
    if tokio::fs::metadata(&script_abs).await.is_err() {
        anyhow::bail!("missing Diffusers runner script: {}", script_abs.display());
    }

    // RAII: deletes on drop, including on early return / panic.
    let run_dir = tempfile::Builder::new()
        .prefix("zun-flux2-9b-kv-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("create diffusers run dir: {e}"))?;

    tracing::info!(
        target: "audit",
        event = "job.diffusers_start",
        job_id = %job.id,
        user_id = job.user_id,
        pipeline = PIPELINE,
        model_path = %model_path.display(),
        steps = STEPS,
        width = WIDTH,
        height = HEIGHT,
    );

    let output = Command::new("uv")
        .current_dir(&project_root)
        .arg("run")
        .arg("python")
        .arg(script_rel)
        .arg("--prompt")
        .arg(prompt_text)
        .arg("--input")
        .arg(input_abs)
        .arg("--out")
        .arg(run_dir.path())
        .arg("--seed")
        .arg(job.seed.to_string())
        .arg("--steps")
        .arg(STEPS.to_string())
        .arg("--height")
        .arg(HEIGHT.to_string())
        .arg("--width")
        .arg(WIDTH.to_string())
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "Diffusers 9B-KV runner failed with status {}: stderr={} stdout={}",
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }

    let generated = find_single_png(run_dir.path()).await?;
    let filename = format!("zun_{}.png", job.id);
    let abs_output =
        paths::user_data_path(&state.config.data_dir, user, subdir::OUTPUTS, &filename)?;
    if let Some(parent) = abs_output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    paths::atomic_copy(&generated, &abs_output).await?;
    let bytes_len = tokio::fs::metadata(&abs_output).await?.len() as usize;
    let rel_output = relative_for_db(&abs_output, &state.config.data_dir);

    let metadata = serde_json::json!({
        "workflow": job.workflow,
        "runtime": "diffusers",
        "pipeline": PIPELINE,
        "model_path": model_path.to_string_lossy(),
        "dtype": DTYPE,
        "device": "cuda",
        "offload_mode": OFFLOAD_MODE,
        "seed": job.seed,
        "steps": STEPS,
        "width": WIDTH,
        "height": HEIGHT,
        "input_image": {
            "path": input_rel,
            "original_name": input_original_name,
        },
        "generated_path": generated.to_string_lossy(),
    });
    let sidecar = abs_output.with_extension("json");
    paths::atomic_write(&sidecar, &serde_json::to_vec_pretty(&metadata)?).await?;

    finalize_output(
        state,
        job,
        user,
        &abs_output,
        &rel_output,
        bytes_len,
        started_at,
    )
    .await
}

async fn find_single_png(dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut pngs = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            pngs.push(path);
        }
    }
    match pngs.len() {
        1 => Ok(pngs.remove(0)),
        0 => anyhow::bail!(
            "Diffusers runner did not produce a PNG in {}",
            dir.display()
        ),
        n => anyhow::bail!("Diffusers runner produced {n} PNGs in {}", dir.display()),
    }
}

async fn finalize_output(
    state: &AppState,
    job: &QueuedJob,
    user: UserId,
    abs_output: &std::path::Path,
    rel_output: &str,
    output_bytes: usize,
    started_at: std::time::Instant,
) -> anyhow::Result<()> {
    let abs_output_for_read = abs_output.to_path_buf();
    let dim_result = tokio::task::spawn_blocking(move || -> anyhow::Result<(u32, u32)> {
        let reader = image::ImageReader::open(&abs_output_for_read)?;
        Ok(reader.into_dimensions()?)
    })
    .await;
    let (width, height) = match dim_result {
        Ok(Ok((w, h))) => (Some(w as i64), Some(h as i64)),
        Ok(Err(e)) => {
            tracing::warn!(
                job_id = %job.id,
                output = %rel_output,
                error = %e,
                "failed to read output dimensions; storing as null",
            );
            (None, None)
        }
        Err(e) => {
            tracing::warn!(
                job_id = %job.id,
                error = %e,
                "dimension-read task join failed; storing as null",
            );
            (None, None)
        }
    };

    if !mark_done(&state.db, &job.id, rel_output, width, height).await? {
        tracing::info!(
            target: "audit",
            event = "job.completion_discarded",
            job_id = %job.id,
            user_id = job.user_id,
            "generation completed but row no longer running; cancellation/delete won the race",
        );
        return Ok(());
    }

    // Eager render of thumb + preview so the phone never pays for encode
    // latency on first view. Failures are logged inside the helper and
    // never bubble up — the job is already done.
    crate::derived_images::generate_for_job(
        &state.db,
        &state.config.data_dir,
        user,
        &job.id,
        abs_output,
    )
    .await;

    tracing::info!(
        target: "audit",
        event = "job.done",
        job_id = %job.id,
        user_id = job.user_id,
        output = %rel_output,
        output_bytes,
        width = ?width,
        height = ?height,
        duration_ms = started_at.elapsed().as_millis() as u64,
    );
    Ok(())
}

/// Resolve `(prompt_text, timeout_seconds)` from `prompt_id` (DB lookup) or
/// `prompt_text` (free-text on the job row itself).
async fn resolve_prompt_and_timeout(
    state: &AppState,
    job: &QueuedJob,
) -> anyhow::Result<(String, u64)> {
    if let Some(pid) = job.prompt_id {
        // Note: not filtering on deleted_at — a job submitted before the
        // prompt was deleted should still run. The handler validated at
        // submit time that the prompt existed and was not deleted.
        let row: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT text, timeout_seconds FROM custom_prompts \
             WHERE id = ? AND user_id = ?",
        )
        .bind(pid)
        .bind(job.user_id)
        .fetch_optional(&state.db)
        .await?;
        let (text, timeout) = row.ok_or_else(|| anyhow::anyhow!("prompt id {pid} disappeared"))?;
        Ok((
            text,
            timeout
                .map(|t| t as u64)
                .unwrap_or(crate::DEFAULT_TIMEOUT_SECONDS),
        ))
    } else {
        let text = job.prompt_text.clone().ok_or_else(|| {
            anyhow::anyhow!("job {} missing both prompt_id and prompt_text", job.id)
        })?;
        Ok((text, crate::DEFAULT_TIMEOUT_SECONDS))
    }
}

fn relative_for_db(abs: &std::path::Path, data_dir: &std::path::Path) -> String {
    abs.strip_prefix(data_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs.to_string_lossy().into_owned())
}
