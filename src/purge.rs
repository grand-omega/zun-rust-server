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

    /// Like [`Self::defaults_now`], with both windows set from config's
    /// `purge_after_days`.
    pub fn for_retention_days_now(days: u32) -> Self {
        let secs = i64::from(days) * 24 * 60 * 60;
        Self {
            delete_grace_seconds: secs,
            cache_ttl_seconds: secs,
            ..Self::defaults_now()
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
            let opts = PurgeOpts::for_retention_days_now(state.config.purge_after_days);
            match run(&state, opts).await {
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
    type StaleJobRow = (String, Option<String>, Option<String>, Option<String>);
    let stale_jobs: Vec<StaleJobRow> = sqlx::query_as(
        "SELECT id, output_path, thumb_path, preview_path FROM jobs \
         WHERE deleted_at IS NOT NULL AND deleted_at < ?",
    )
    .bind(job_cutoff)
    .fetch_all(&state.db)
    .await?;

    for (id, output_path, thumb_path, preview_path) in stale_jobs {
        for rel in [
            output_path.as_deref(),
            thumb_path.as_deref(),
            preview_path.as_deref(),
        ]
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
