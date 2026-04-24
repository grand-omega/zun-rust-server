//! Background worker that drains queued jobs through ComfyUI.
//!
//! Lifecycle per job:
//! 1. Reset `running` rows left over from a previous crash (on startup).
//! 2. Pick the oldest `queued` row.
//! 3. Mark it `running`, upload the input, submit the patched workflow,
//!    poll `/history` until it materialises (bounded by a per-job timeout),
//!    download the primary output, write to disk, mark `done`.
//! 4. Any error → mark `failed` with message; move on.
//!
//! Concurrency: exactly one job at a time. FLUX2 saturates the GPU.

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::{mpsc, watch};

use crate::{
    AppState, comfy::ComfyClient, comfy::HistoryEntry, prompts::CUSTOM_PROMPT_ID, workflow,
};

const POLL_INTERVAL: Duration = Duration::from_millis(1000);
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

        // Drain the queue as long as there are jobs to process.
        loop {
            if *shutdown.borrow() {
                tracing::info!("worker shutting down (mid-drain)");
                return;
            }
            match fetch_oldest_queued(&state.db).await {
                Ok(Some(job)) => {
                    let job_id = job.id.clone();
                    let prompt_id = job.prompt_id.clone();
                    // Note: if shutdown fires while process_job is running,
                    // we let that job finish. Shutdown is checked on the
                    // next iteration. This is intentional — interrupting
                    // ComfyUI mid-execution leaves orphaned GPU state.
                    if let Err(e) = process_job(&state, &job).await {
                        tracing::error!(
                            target: "audit",
                            event = "job.failed",
                            job_id = %job_id,
                            %prompt_id,
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

        // Wait for a new submission, idle tick, or shutdown.
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
    prompt_id: String,
    input_path: String,
    custom_prompt: Option<String>,
}

async fn fetch_oldest_queued(db: &SqlitePool) -> anyhow::Result<Option<QueuedJob>> {
    let row = sqlx::query_as::<_, QueuedJob>(
        "SELECT id, prompt_id, input_path, custom_prompt FROM jobs \
         WHERE status = 'queued' ORDER BY created_at ASC LIMIT 1",
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

async fn mark_running(db: &SqlitePool, job_id: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE jobs SET status = 'running', started_at = ? WHERE id = ?")
        .bind(now)
        .bind(job_id)
        .execute(db)
        .await?;
    Ok(())
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

async fn mark_done(
    db: &SqlitePool,
    job_id: &str,
    output_path: &str,
    width: Option<i64>,
    height: Option<i64>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE jobs SET status = 'done', output_path = ?, completed_at = ?, \
         width = ?, height = ? WHERE id = ?",
    )
    .bind(output_path)
    .bind(now)
    .bind(width)
    .bind(height)
    .bind(job_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn mark_failed(db: &SqlitePool, job_id: &str, error_message: &str) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE jobs SET status = 'failed', error_message = ?, completed_at = ? WHERE id = ?",
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
    mark_running(&state.db, &job.id).await?;
    tracing::info!(
        target: "audit",
        event = "job.running",
        job_id = %job.id,
        prompt_id = %job.prompt_id,
    );

    // Resolve prompt + workflow template.
    let prompt = state
        .prompts
        .get(&job.prompt_id)
        .ok_or_else(|| anyhow::anyhow!("unknown prompt_id: {}", job.prompt_id))?;
    let template = state
        .workflows
        .get(&prompt.workflow)
        .ok_or_else(|| anyhow::anyhow!("workflow template missing: {}", prompt.workflow))?;

    // Read input bytes from disk.
    let input_abs = state.config.data_dir.join(&job.input_path);
    let input_bytes = tokio::fs::read(&input_abs)
        .await
        .map_err(|e| anyhow::anyhow!("read input {}: {e}", input_abs.display()))?;

    // Upload to ComfyUI — filename is what the LoadImage node will reference.
    let ext = std::path::Path::new(&job.input_path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg");
    let upload_name = format!("zun_{}.{ext}", job.id);
    let stored_name = state.comfy.upload_image(input_bytes, &upload_name).await?;

    // Patch workflow and submit.
    let prompt_text = if job.prompt_id == CUSTOM_PROMPT_ID {
        job.custom_prompt.as_deref().ok_or_else(|| {
            anyhow::anyhow!("__custom__ job {} is missing custom_prompt text", job.id)
        })?
    } else {
        &prompt.text
    };
    let patched = workflow::build_edit_workflow(template, prompt_text, &stored_name, &job.id);
    let comfy_prompt_id = state.comfy.submit_prompt(&patched).await?;
    update_comfy_prompt_id(&state.db, &job.id, &comfy_prompt_id).await?;
    tracing::info!(job_id = %job.id, comfy_prompt_id = %comfy_prompt_id, "submitted to comfyui");

    // Poll /history, bounded by the per-prompt job timeout.
    let timeout = Duration::from_secs(prompt.timeout_seconds);
    let entry = tokio::time::timeout(timeout, poll_until_history(&state.comfy, &comfy_prompt_id))
        .await
        .map_err(|_| anyhow::anyhow!("comfyui timeout after {}s", prompt.timeout_seconds))??;

    if !entry.succeeded() {
        let status_str = entry.status.status_str.as_deref().unwrap_or("unknown");
        anyhow::bail!("comfyui execution failed (status={status_str})");
    }

    // Pick our output (filtering out mask_preview_* side outputs).
    let prefix = format!("zun_{}", job.id);
    let output_img = entry
        .primary_output(&prefix)
        .ok_or_else(|| {
            anyhow::anyhow!("no output image matched prefix `{prefix}` in comfy history")
        })?
        .clone();

    // Download and save.
    let bytes = state
        .comfy
        .view(
            &output_img.filename,
            &output_img.subfolder,
            &output_img.r#type,
        )
        .await?;
    let rel_output = format!("outputs/{}", output_img.filename);
    let abs_output = state.config.data_dir.join(&rel_output);
    if let Some(parent) = abs_output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&abs_output, &bytes).await?;

    // Cheap dimension read via image's ImageReader::into_dimensions;
    // non-fatal if the file isn't a decodable image (shouldn't happen
    // since ComfyUI produces PNGs, but we don't want this to fail the job).
    // Log at warn instead of silently swallowing so a pipeline regression
    // producing non-decodable outputs is visible.
    let abs_output_for_read = abs_output.clone();
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

    mark_done(&state.db, &job.id, &rel_output, width, height).await?;
    tracing::info!(
        target: "audit",
        event = "job.done",
        job_id = %job.id,
        prompt_id = %job.prompt_id,
        output = %rel_output,
        output_bytes = bytes.len(),
        width = ?width,
        height = ?height,
        duration_ms = started_at.elapsed().as_millis() as u64,
    );
    Ok(())
}

async fn poll_until_history(comfy: &ComfyClient, prompt_id: &str) -> anyhow::Result<HistoryEntry> {
    loop {
        if let Some(entry) = comfy.get_history(prompt_id).await? {
            return Ok(entry);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
