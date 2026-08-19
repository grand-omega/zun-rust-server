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

pub fn spawn(
    pool: SqlitePool,
    data_dir: std::path::PathBuf,
    keep_days: u32,
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
            if let Err(e) = prune_old(&data_dir, keep_days).await {
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
    // VACUUM INTO refuses to overwrite an existing file, and a crash
    // mid-VACUUM would otherwise leave a corrupt file at today's dated
    // name for up to 24h. Write to a temp sibling instead and rename into
    // place, same crash-safety as `paths::atomic_write`.
    // `Staged` removes the temp file if either step below fails; nothing
    // else would (see its docs — `prune_old`'s `.db` filter never matches a
    // `.tmp.` name).
    let mut staged = crate::paths::Staged::new(&abs);

    // SQLite doesn't accept a parameter binding for the destination path,
    // so we string-substitute. data_dir is admin-controlled config; we still
    // escape single quotes defensively.
    let escaped = staged.path().to_string_lossy().replace('\'', "''");
    let sql = format!("VACUUM INTO '{escaped}'");
    sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
    tokio::fs::rename(staged.path(), &abs).await?;
    staged.commit();
    Ok(abs)
}

async fn prune_old(data_dir: &std::path::Path, keep_days: u32) -> anyhow::Result<()> {
    let dir = data_dir.join("backups");
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let cutoff =
        std::time::SystemTime::now() - Duration::from_secs(u64::from(keep_days) * 24 * 60 * 60);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_once_writes_valid_db_with_no_leftover_tmp_file() {
        // A real file-backed DB, not `sqlite::memory:` — VACUUM INTO
        // against an in-memory connection is unreliable in this sqlx
        // version (returns Ok without writing the destination file), which
        // is irrelevant to the production path (always a real jobs.db).
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(dir.path()).await.unwrap();
        let path = snapshot_once(&pool, dir.path()).await.unwrap();

        assert!(tokio::fs::metadata(&path).await.is_ok());
        let mut entries = tokio::fs::read_dir(path.parent().unwrap()).await.unwrap();
        let mut count = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            assert!(
                !e.file_name().to_string_lossy().contains(".tmp."),
                "leftover temp file: {:?}",
                e.file_name()
            );
            count += 1;
        }
        assert_eq!(count, 1, "exactly the dated snapshot, nothing else");
    }

    #[tokio::test]
    async fn snapshot_once_overwrites_same_day_snapshot_via_rename() {
        // Regression test for SEC-006: the old implementation pre-deleted
        // the destination file before VACUUM INTO; the fixed version relies
        // on `tokio::fs::rename` replacing the destination atomically, so
        // calling this twice in the same day must still succeed.
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(dir.path()).await.unwrap();
        let first = snapshot_once(&pool, dir.path()).await.unwrap();
        let second = snapshot_once(&pool, dir.path()).await.unwrap();
        assert_eq!(first, second);
        assert!(tokio::fs::metadata(&second).await.is_ok());
    }

    #[tokio::test]
    async fn prune_old_removes_only_db_files_older_than_keep_days() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        tokio::fs::create_dir_all(&backups).await.unwrap();

        let old_db = backups.join("jobs-2000-01-01.db");
        let recent_db = backups.join("jobs-2999-01-01.db");
        let non_db = backups.join("notes.txt");
        tokio::fs::write(&old_db, b"old").await.unwrap();
        tokio::fs::write(&recent_db, b"recent").await.unwrap();
        tokio::fs::write(&non_db, b"keep me").await.unwrap();

        // Backdate the "old" file's mtime past the retention window.
        let old_time = std::time::SystemTime::now() - Duration::from_secs(400 * 24 * 60 * 60);
        std::fs::File::open(&old_db)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        prune_old(dir.path(), 30).await.unwrap();

        assert!(
            tokio::fs::metadata(&old_db).await.is_err(),
            "old .db file should be pruned"
        );
        assert!(
            tokio::fs::metadata(&recent_db).await.is_ok(),
            "recent .db file should survive"
        );
        assert!(
            tokio::fs::metadata(&non_db).await.is_ok(),
            "non-.db file should never be touched"
        );
    }
}
