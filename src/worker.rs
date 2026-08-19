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
use tokio::sync::{mpsc, watch};

use crate::{
    AppState, comfy,
    paths::{self, subdir},
    workflow,
};

const IDLE_TICK: Duration = Duration::from_secs(30);

/// How long to sit out after finding ComfyUI unreachable, before the worker
/// looks at the queue again. Long enough that a backend restart passes
/// quietly, short enough that the queue resumes without operator action.
const UNREACHABLE_BACKOFF: Duration = Duration::from_secs(10);

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
                        // "ComfyUI isn't there" is not this job's fault.
                        // Failing on it meant a backend restart marked every
                        // queued job `failed` in a few seconds — each one
                        // burning its retries and dying ~1.5s apart, with no
                        // way to get them back. Put the row back and wait.
                        if comfy::is_unreachable(&e) {
                            tracing::warn!(
                                target: "audit",
                                event = "job.requeued_backend_down",
                                job_id = %job_id,
                                error = %e,
                                backoff_s = UNREACHABLE_BACKOFF.as_secs(),
                            );
                            if let Err(req_err) = requeue(&state.db, &job_id).await {
                                tracing::error!(job_id = %job_id, error = ?req_err, "could not requeue job");
                            }
                            notify_job_change(&state);
                            tokio::time::sleep(UNREACHABLE_BACKOFF).await;
                            break;
                        }
                        tracing::error!(
                            target: "audit",
                            event = "job.failed",
                            job_id = %job_id,
                            error = ?e,
                            "job failed",
                        );
                        // The audit line above carries the full, unredacted
                        // chain for the operator. What gets persisted is what
                        // the client reads back from `GET /jobs/{id}`, so it
                        // goes through the same redaction as a 5xx body —
                        // otherwise a failed job hands the phone the raw
                        // comfy_url and data_dir paths that `AppError` is
                        // careful to strip.
                        let stored = crate::error::redact_internal(&format!("{e:#}"));
                        if let Err(mark_err) = mark_failed(&state.db, &job_id, &stored).await {
                            tracing::error!(job_id = %job_id, error = ?mark_err, "could not mark job failed");
                        }
                    }
                    notify_job_change(&state);
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

/// How long to keep waiting for ComfyUI to answer before giving up on
/// warming it. Covers "both units started at boot and ComfyUI is still
/// importing custom nodes" without hanging around forever if it never comes.
const WARMUP_WAIT_LIMIT: Duration = Duration::from_secs(300);
/// Gap between reachability probes while waiting for the backend.
const WARMUP_PROBE_INTERVAL: Duration = Duration::from_secs(3);

/// Run one throwaway generation so the first *real* job doesn't pay for
/// loading the model.
///
/// Measured on an RTX 4070 Ti Super: the first job after a ComfyUI restart
/// takes ~20 s because it stages ~4 GB of weights, against ~3 s once they
/// are resident. That 17 s lands squarely on the user's first tap. This
/// moves it to boot, where nobody is waiting.
///
/// Best-effort throughout: every failure is logged and dropped. A backend
/// that never comes up, a missing model, a workflow that no longer matches
/// the template — none of it should stop the server from serving.
pub fn spawn_warmup(
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + WARMUP_WAIT_LIMIT;
        while !state.comfy.reachable(WARMUP_PROBE_INTERVAL).await {
            if *shutdown.borrow() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    waited_s = WARMUP_WAIT_LIMIT.as_secs(),
                    "comfyui never became reachable; skipping warm-up",
                );
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(WARMUP_PROBE_INTERVAL) => {}
                _ = shutdown.changed() => if *shutdown.borrow() { return },
            }
        }
        match warm_once(&state).await {
            Ok(elapsed_ms) => tracing::info!(
                target: "audit",
                event = "warmup.done",
                elapsed_ms,
                "comfyui warmed; first real job skips the model load",
            ),
            Err(e) => tracing::warn!(error = %e, "comfyui warm-up failed (non-fatal)"),
        }
    })
}

/// Submit one minimal generation through the default workflow and wait for
/// it to finish. Deliberately uses the same path a real job takes — a
/// partial workflow would not force the sampler to pull the weights into
/// VRAM, which is the whole point.
async fn warm_once(state: &AppState) -> anyhow::Result<u64> {
    let started = std::time::Instant::now();
    let template = state
        .workflows
        .supported_template(workflow::DEFAULT_WORKFLOW)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let stored_name = state
        .comfy
        .upload_image(warmup_png()?, "zun_warmup.png")
        .await?;
    let patched = workflow::build_edit_workflow(template, "warmup", &stored_name, "warmup", 1);

    let client_id = uuid::Uuid::new_v4().to_string();
    let mut ws = state.comfy.connect_ws(&client_id).await?;
    let prompt_id = state.comfy.submit_prompt(&patched, &client_id).await?;
    tokio::time::timeout(
        Duration::from_secs(crate::MAX_TIMEOUT_SECONDS as u64),
        comfy::await_completion(&mut ws, &prompt_id, None),
    )
    .await
    .map_err(|_| anyhow::anyhow!("warm-up timed out"))??;
    Ok(started.elapsed().as_millis() as u64)
}

/// The smallest valid PNG we can feed the workflow's LoadImage node.
fn warmup_png() -> anyhow::Result<Vec<u8>> {
    let img = image::RgbImage::from_pixel(64, 64, image::Rgb([128, 128, 128]));
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
    Ok(buf)
}

/// Wake anything blocked in a long-poll. Cheap and lossless: `watch` keeps
/// only the latest value, and a waiter re-reads the DB before deciding.
pub(crate) fn notify_job_change(state: &AppState) {
    state.job_events.send_modify(|n| *n = n.wrapping_add(1));
}

#[derive(sqlx::FromRow)]
struct QueuedJob {
    id: String,
    input_id: i64,
    prompt_text: String,
    workflow: String,
    timeout_seconds: Option<i64>,
    seed: i64,
}

async fn fetch_oldest_queued(db: &SqlitePool) -> anyhow::Result<Option<QueuedJob>> {
    // Skip soft-deleted rows: a DELETE on a queued job leaves status='queued'
    // but flips deleted_at — without this filter the worker would still pick
    // it up and burn GPU on output the user can't read back.
    let row = sqlx::query_as::<_, QueuedJob>(
        "SELECT id, input_id, prompt_text, workflow, timeout_seconds, seed \
         FROM jobs WHERE status = 'queued' AND deleted_at IS NULL \
         ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row)
}

async fn reset_running_jobs(db: &SqlitePool) -> anyhow::Result<()> {
    // Also clear per-attempt state from the prior crashed run so duration
    // math and audit logs reflect the new attempt, not the dead one.
    let result = sqlx::query(
        "UPDATE jobs SET status = 'queued', started_at = NULL, comfy_prompt_id = NULL, error_message = NULL, progress = 0.0 \
         WHERE status = 'running'",
    )
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
    // Also gate on deleted_at: closes the small window between
    // fetch_oldest_queued and here in case the row got soft-deleted in between.
    let res = sqlx::query(
        "UPDATE jobs SET status = 'running', started_at = ?, progress = 0.0 \
         WHERE id = ? AND status = 'queued' AND deleted_at IS NULL",
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
/// `running` (e.g. the job was cancelled while ComfyUI was busy); caller should
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
         width = ?, height = ?, progress = 1.0 WHERE id = ? AND status = 'running'",
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

/// Put a claimed job back on the queue after the backend turned out to be
/// unreachable. Gated on `status = 'running'` like every other transition,
/// so a cancel that landed in the meantime wins.
async fn requeue(db: &SqlitePool, job_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE jobs SET status = 'queued', started_at = NULL, comfy_prompt_id = NULL, \
         progress = 0.0 WHERE id = ? AND status = 'running'",
    )
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn mark_failed(db: &SqlitePool, job_id: &str, error_message: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    // Gated on status='running': if the job was cancelled concurrently,
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
    if !mark_running(&state.db, &job.id).await? {
        // Lost the queued→unclaimable race: the row was either cancelled,
        // hard-deleted, or soft-deleted between fetch and now. Drop the job
        // silently — there's nothing to do, and mark_failed would overwrite
        // the new status.
        tracing::info!(
            target: "audit",
            event = "job.skipped_unclaimable",
            job_id = %job.id,
        );
        return Ok(());
    }
    notify_job_change(state);
    tracing::info!(
        target: "audit",
        event = "job.running",
        job_id = %job.id,
        workflow = %job.workflow,
        seed = job.seed,
    );

    let prompt_text = &job.prompt_text;
    let timeout_seconds = crate::clamp_timeout_seconds(job.timeout_seconds);

    // Read input bytes from the cache by input_id.
    let input_row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT path, sha256 FROM inputs WHERE id = ?")
            .bind(job.input_id)
            .fetch_optional(&state.db)
            .await?;
    let (input_path, input_sha) =
        input_row.ok_or_else(|| anyhow::anyhow!("input row {} disappeared", job.input_id))?;
    let input_rel = input_path
        .ok_or_else(|| anyhow::anyhow!("input file purged for input_id {}", job.input_id))?;
    let input_abs = state.config.data_dir.join(&input_rel);

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
    // Name the upload after the input's content hash, not the job id.
    // ComfyUI never prunes its `input/` dir, and a job-id name meant running
    // five prompts against one photo left five identical copies there
    // forever. With `overwrite=true` a content-addressed name is idempotent,
    // so the directory grows with distinct inputs instead of with jobs.
    // (The *output* prefix stays job-scoped — `primary_output` matches on it.)
    let upload_name = format!("zun_{input_sha}.{ext}");
    let stored_name = state.comfy.upload_image(input_bytes, &upload_name).await?;

    // Patch workflow: prompt + image + filename prefix + seed.
    let patched =
        workflow::build_edit_workflow(template, prompt_text, &stored_name, &job.id, job.seed);

    // Open ws BEFORE submit so we don't miss events between queue and execute.
    let client_id = uuid::Uuid::new_v4().to_string();
    let mut ws = state.comfy.connect_ws(&client_id).await?;

    let comfy_prompt_id = state.comfy.submit_prompt(&patched, &client_id).await?;
    update_comfy_prompt_id(&state.db, &job.id, &comfy_prompt_id).await?;
    tracing::info!(job_id = %job.id, comfy_prompt_id = %comfy_prompt_id, "submitted to comfyui");

    // Forward ws progress frames into the jobs row so polling clients see
    // real percentages. A separate writer task keeps DB latency out of the
    // ws read loop; writes are throttled to ~2% steps.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<f32>();
    let progress_db = state.db.clone();
    let progress_job_id = job.id.clone();
    let progress_events = state.job_events.clone();
    let progress_writer = tokio::spawn(async move {
        let mut last_written = 0.0f32;
        while let Some(p) = progress_rx.recv().await {
            // Multi-node workflows reset per-node progress; write decreases
            // through so the bar tracks reality instead of freezing.
            if p < last_written || p - last_written >= 0.02 {
                last_written = p;
                let _ =
                    sqlx::query("UPDATE jobs SET progress = ? WHERE id = ? AND status = 'running'")
                        .bind(p)
                        .bind(&progress_job_id)
                        .execute(&progress_db)
                        .await;
                progress_events.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
    });

    let timeout = Duration::from_secs(timeout_seconds);
    let completion = tokio::time::timeout(
        timeout,
        comfy::await_completion(&mut ws, &comfy_prompt_id, Some(&progress_tx)),
    )
    .await;
    drop(progress_tx);
    let _ = progress_writer.await;
    let Ok(completion) = completion else {
        // Our timeout expiring does not stop ComfyUI: the prompt keeps
        // running, holds the GPU, and its output is never collected. The
        // next job would then queue *inside* ComfyUI behind a prompt this
        // server has already given up on, so repeated timeouts stack.
        // Interrupt explicitly, same as `cancel_job` does.
        if let Err(e) = state.comfy.interrupt().await {
            tracing::warn!(job_id = %job.id, error = %e, "comfy /interrupt after timeout failed");
        }
        anyhow::bail!("comfyui timeout after {timeout_seconds}s");
    };
    completion?;

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
    let abs_output = paths::data_path(
        &state.config.data_dir,
        subdir::OUTPUTS,
        &output_img.filename,
    )?;
    if let Some(parent) = abs_output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    paths::atomic_write(&abs_output, &bytes).await?;
    let rel_output = paths::relative_for_db(&abs_output, &state.config.data_dir);

    finalize_output(
        state,
        job,
        &abs_output,
        &rel_output,
        bytes.len(),
        started_at,
    )
    .await
}

async fn finalize_output(
    state: &AppState,
    job: &QueuedJob,
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
            "generation completed but row no longer running; cancellation/delete won the race",
        );
        return Ok(());
    }

    tracing::info!(
        target: "audit",
        event = "job.done",
        job_id = %job.id,
        output = %rel_output,
        output_bytes,
        width = ?width,
        height = ?height,
        duration_ms = started_at.elapsed().as_millis() as u64,
    );

    // Render thumb + preview off the worker's critical path. Encoding the
    // four renditions costs ~2.5 s (release) against ~3.3 s of GPU time, so
    // awaiting it here left the card idle for nearly half of each job's wall
    // clock and made the next queued job wait on JPEG/AVIF encoding.
    // Detached, the worker goes straight back for the next job.
    //
    // Nothing depends on this finishing: the image handlers lazily generate
    // any rendition that is missing (`derived_images::ensure_one`), so a
    // failure here, or a shutdown that kills the task mid-encode, costs one
    // slow first view and nothing else.
    let db = state.db.clone();
    let data_dir = state.config.data_dir.clone();
    let job_id = job.id.clone();
    let output_for_render = abs_output.to_path_buf();
    tokio::spawn(async move {
        crate::derived_images::generate_for_job(&db, &data_dir, &job_id, &output_for_render).await;
    });

    Ok(())
}
