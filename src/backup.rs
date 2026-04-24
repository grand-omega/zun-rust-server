//! Daily snapshot of `jobs.db` using SQLite's `VACUUM INTO`. Self-contained:
//! no external `cron` or `sqlite3` CLI required. Snapshots land at
//! `data/backups/jobs-YYYY-MM-DD.db` and rotate on a `RETENTION_DAYS` window.
//!
//! `VACUUM INTO` produces a defragmented, point-in-time copy of the entire
//! database without holding write locks. Safe to run while the server is
//! actively accepting traffic.

use std::time::Duration;

use chrono::{Datelike, Utc};
use sqlx::SqlitePool;
use tokio::sync::watch;

const TICK: Duration = Duration::from_secs(24 * 60 * 60);
const RETENTION_DAYS: u64 = 30;

pub fn spawn(
    pool: SqlitePool,
    data_dir: std::path::PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                return;
            }
            match snapshot_once(&pool, &data_dir).await {
                Ok(path) => tracing::info!(
                    target: "audit",
                    event = "backup.done",
                    path = %path.display(),
                ),
                Err(e) => tracing::error!(error = %e, "backup snapshot failed"),
            }
            if let Err(e) = prune_old(&data_dir).await {
                tracing::warn!(error = %e, "backup prune failed");
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

/// Run one backup snapshot. Returns the absolute path of the written file.
pub async fn snapshot_once(
    pool: &SqlitePool,
    data_dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let dir = data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let now = Utc::now();
    let filename = format!(
        "jobs-{:04}-{:02}-{:02}.db",
        now.year(),
        now.month(),
        now.day()
    );
    let abs = dir.join(&filename);

    // VACUUM INTO refuses to overwrite. If today's snapshot exists, delete first.
    if tokio::fs::metadata(&abs).await.is_ok() {
        tokio::fs::remove_file(&abs).await?;
    }

    // SQLite doesn't accept a parameter binding for the destination path,
    // so we string-substitute. data_dir is admin-controlled config; we still
    // escape single quotes defensively.
    let escaped = abs.to_string_lossy().replace('\'', "''");
    let sql = format!("VACUUM INTO '{escaped}'");
    sqlx::query(&sql).execute(pool).await?;
    Ok(abs)
}

async fn prune_old(data_dir: &std::path::Path) -> anyhow::Result<()> {
    let dir = data_dir.join("backups");
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(RETENTION_DAYS * 24 * 60 * 60);
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("db") {
            continue;
        }
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = match meta.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if modified < cutoff
            && let Err(e) = tokio::fs::remove_file(&path).await
        {
            tracing::warn!(path = %path.display(), error = %e, "could not delete stale backup");
        }
    }
    Ok(())
}
