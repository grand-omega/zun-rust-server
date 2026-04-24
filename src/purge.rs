//! Daily housekeeping:
//! - Hard-delete soft-deleted jobs older than `delete_grace_seconds` (default
//!   30 days). Removes output + thumb files and the row.
//! - Nullify `inputs.path` and delete cache files where `last_used_at` is
//!   older than `cache_ttl_seconds` (default 30 days) and no active job
//!   references them.

use std::time::Duration;

use tokio::sync::watch;

use crate::AppState;

/// Default grace period for soft-deleted jobs (30 days, in seconds).
pub const DEFAULT_DELETE_GRACE_SECS: i64 = 30 * 24 * 60 * 60;
/// Default TTL for unused input cache files (30 days, in seconds).
pub const DEFAULT_CACHE_TTL_SECS: i64 = 30 * 24 * 60 * 60;

const TICK: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, Clone, Copy)]
pub struct PurgeReport {
    pub jobs_hard_deleted: usize,
    pub job_files_removed: usize,
    pub inputs_purged: usize,
    pub input_files_removed: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PurgeOpts {
    pub now_seconds: i64,
    pub delete_grace_seconds: i64,
    pub cache_ttl_seconds: i64,
    pub dry_run: bool,
}

impl PurgeOpts {
    pub fn defaults_now() -> Self {
        Self {
            now_seconds: chrono::Utc::now().timestamp(),
            delete_grace_seconds: DEFAULT_DELETE_GRACE_SECS,
            cache_ttl_seconds: DEFAULT_CACHE_TTL_SECS,
            dry_run: false,
        }
    }
}

/// Spawn the daily purge task. First tick fires immediately on startup.
pub fn spawn(state: AppState, mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                return;
            }
            match run(&state, PurgeOpts::defaults_now()).await {
                Ok(report) => {
                    if report.jobs_hard_deleted + report.inputs_purged > 0 {
                        tracing::info!(
                            target: "audit",
                            event = "purge.done",
                            jobs_hard_deleted = report.jobs_hard_deleted,
                            job_files_removed = report.job_files_removed,
                            inputs_purged = report.inputs_purged,
                            input_files_removed = report.input_files_removed,
                        );
                    }
                }
                Err(e) => tracing::error!(error = %e, "purge run failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    })
}

pub async fn run(state: &AppState, opts: PurgeOpts) -> anyhow::Result<PurgeReport> {
    let mut report = PurgeReport::default();

    let job_cutoff = opts.now_seconds - opts.delete_grace_seconds;
    let input_cutoff = opts.now_seconds - opts.cache_ttl_seconds;

    // Soft-deleted jobs older than the grace period.
    let stale_jobs: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, output_path, thumb_path FROM jobs \
         WHERE deleted_at IS NOT NULL AND deleted_at < ?",
    )
    .bind(job_cutoff)
    .fetch_all(&state.db)
    .await?;

    for (id, output_path, thumb_path) in stale_jobs {
        for rel in [output_path.as_deref(), thumb_path.as_deref()]
            .into_iter()
            .flatten()
        {
            let abs = state.config.data_dir.join(rel);
            if opts.dry_run {
                report.job_files_removed += 1;
                continue;
            }
            match tokio::fs::remove_file(&abs).await {
                Ok(()) => report.job_files_removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(path = %abs.display(), error = %e, "purge: failed to remove file")
                }
            }
        }
        if !opts.dry_run {
            sqlx::query("DELETE FROM jobs WHERE id = ?")
                .bind(&id)
                .execute(&state.db)
                .await?;
        }
        report.jobs_hard_deleted += 1;
    }

    // Inputs whose file is stale AND no non-deleted job references them.
    let stale_inputs: Vec<(i64, String)> = sqlx::query_as(
        "SELECT i.id, i.path FROM inputs i \
         WHERE i.path IS NOT NULL AND i.last_used_at < ? \
           AND NOT EXISTS ( \
             SELECT 1 FROM jobs j \
             WHERE j.input_id = i.id AND j.deleted_at IS NULL \
           )",
    )
    .bind(input_cutoff)
    .fetch_all(&state.db)
    .await?;

    for (id, rel) in stale_inputs {
        let abs = state.config.data_dir.join(&rel);
        if opts.dry_run {
            report.input_files_removed += 1;
            report.inputs_purged += 1;
            continue;
        }
        match tokio::fs::remove_file(&abs).await {
            Ok(()) => report.input_files_removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %abs.display(), error = %e, "purge: failed to remove cache file")
            }
        }
        sqlx::query("UPDATE inputs SET path = NULL WHERE id = ?")
            .bind(id)
            .execute(&state.db)
            .await?;
        report.inputs_purged += 1;
    }

    Ok(report)
}

/// Pool-only entrypoint that skips file IO. Useful for fast tests and the
/// dry-run path of zun-admin's purge subcommand when no AppState is handy.
#[allow(dead_code)]
pub async fn run_with_pool(
    pool: &sqlx::SqlitePool,
    opts: PurgeOpts,
) -> anyhow::Result<PurgeReport> {
    // Test-only entrypoint that doesn't need the full AppState; uses pool only.
    // Files aren't cleaned in this entry — tests for file removal go through
    // the full `run` with a real AppState.
    let mut report = PurgeReport::default();
    let job_cutoff = opts.now_seconds - opts.delete_grace_seconds;
    let input_cutoff = opts.now_seconds - opts.cache_ttl_seconds;

    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE deleted_at IS NOT NULL AND deleted_at < ?",
    )
    .bind(job_cutoff)
    .fetch_one(pool)
    .await?;
    if !opts.dry_run {
        sqlx::query("DELETE FROM jobs WHERE deleted_at IS NOT NULL AND deleted_at < ?")
            .bind(job_cutoff)
            .execute(pool)
            .await?;
    }
    report.jobs_hard_deleted = n as usize;

    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inputs i WHERE i.path IS NOT NULL AND i.last_used_at < ? \
         AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.input_id = i.id AND j.deleted_at IS NULL)",
    )
    .bind(input_cutoff)
    .fetch_one(pool)
    .await?;
    if !opts.dry_run {
        sqlx::query(
            "UPDATE inputs SET path = NULL WHERE path IS NOT NULL AND last_used_at < ? \
             AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.input_id = inputs.id AND j.deleted_at IS NULL)",
        )
        .bind(input_cutoff)
        .execute(pool)
        .await?;
    }
    report.inputs_purged = n as usize;
    Ok(report)
}
