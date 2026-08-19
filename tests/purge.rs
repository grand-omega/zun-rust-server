//! Integration tests for `purge::run` — hard-deleting stale soft-deleted
//! jobs (and their derived-image files) and clearing unreferenced,
//! stale input cache files. Added per security-audit.md SEC-009: this
//! logic previously had zero automated regression coverage.

use zun_rust_server::purge::{self, PurgeOpts};

mod common;

const DAY: i64 = 24 * 60 * 60;

async fn mark_job_deleted_with_paths(
    db: &sqlx::SqlitePool,
    id: &str,
    deleted_at: i64,
    output_path: &str,
    thumb_path: &str,
    preview_path: &str,
) {
    sqlx::query(
        "UPDATE jobs SET deleted_at = ?, output_path = ?, thumb_path = ?, preview_path = ? \
         WHERE id = ?",
    )
    .bind(deleted_at)
    .bind(output_path)
    .bind(thumb_path)
    .bind(preview_path)
    .bind(id)
    .execute(db)
    .await
    .unwrap();
}

fn write(tempdir: &std::path::Path, rel: &str) {
    let abs = tempdir.join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, b"x").unwrap();
}

fn exists(tempdir: &std::path::Path, rel: &str) -> bool {
    tempdir.join(rel).exists()
}

#[tokio::test]
async fn hard_delete_removes_output_thumb_preview_and_avif_siblings_and_row() {
    let app = common::test_app().await;
    let now = chrono::Utc::now().timestamp();

    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), "a".repeat(64).as_str(), None).await;
    common::seed_job(
        &app.db,
        "job-1",
        "done",
        None,
        None,
        "flux2_klein_edit",
        input_id,
        now - 40 * DAY,
        Some(now - 40 * DAY),
    )
    .await;
    mark_job_deleted_with_paths(
        &app.db,
        "job-1",
        now - 40 * DAY, // deleted well past the default 30-day grace
        "outputs/job-1.jpg",
        "thumbs/job-1.jpg",
        "previews/job-1.jpg",
    )
    .await;

    for rel in [
        "outputs/job-1.jpg",
        "thumbs/job-1.jpg",
        "thumbs/job-1.avif",
        "previews/job-1.jpg",
        "previews/job-1.avif",
    ] {
        write(app._tempdir.path(), rel);
    }

    let opts = PurgeOpts {
        now_seconds: now,
        ..PurgeOpts::defaults_now()
    };
    let report = purge::run(&app.state, opts).await.unwrap();

    assert_eq!(report.jobs_hard_deleted, 1);
    assert_eq!(
        report.job_files_removed, 5,
        "jpeg + avif siblings for thumb and preview, plus output"
    );

    for rel in [
        "outputs/job-1.jpg",
        "thumbs/job-1.jpg",
        "thumbs/job-1.avif",
        "previews/job-1.jpg",
        "previews/job-1.avif",
    ] {
        assert!(!exists(app._tempdir.path(), rel), "{rel} should be removed");
    }

    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM jobs WHERE id = ?")
        .bind("job-1")
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(row.is_none(), "job row should be hard-deleted");
}

#[tokio::test]
async fn stale_unreferenced_input_is_cleared_and_file_removed() {
    let app = common::test_app().await;
    let now = chrono::Utc::now().timestamp();

    let sha = "b".repeat(64);
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(b"bytes")).await;
    sqlx::query("UPDATE inputs SET last_used_at = ? WHERE id = ?")
        .bind(now - 40 * DAY)
        .bind(input_id)
        .execute(&app.db)
        .await
        .unwrap();

    let opts = PurgeOpts {
        now_seconds: now,
        ..PurgeOpts::defaults_now()
    };
    let report = purge::run(&app.state, opts).await.unwrap();

    assert_eq!(report.inputs_purged, 1);
    assert_eq!(report.input_files_removed, 1);
    assert!(
        !exists(app._tempdir.path(), &format!("cache/inputs/{sha}.jpg")),
        "cache file should be removed"
    );

    let (path,): (Option<String>,) = sqlx::query_as("SELECT path FROM inputs WHERE id = ?")
        .bind(input_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(path.is_none(), "path should be cleared to NULL");
}

#[tokio::test]
async fn stale_input_referenced_by_active_job_is_not_purged() {
    let app = common::test_app().await;
    let now = chrono::Utc::now().timestamp();

    let sha = "c".repeat(64);
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(b"bytes")).await;
    sqlx::query("UPDATE inputs SET last_used_at = ? WHERE id = ?")
        .bind(now - 40 * DAY)
        .bind(input_id)
        .execute(&app.db)
        .await
        .unwrap();
    common::seed_job(
        &app.db,
        "job-2",
        "queued",
        None,
        None,
        "flux2_klein_edit",
        input_id,
        now,
        None,
    )
    .await;

    let opts = PurgeOpts {
        now_seconds: now,
        ..PurgeOpts::defaults_now()
    };
    let report = purge::run(&app.state, opts).await.unwrap();

    assert_eq!(
        report.inputs_purged, 0,
        "still referenced by a non-deleted job"
    );
    assert!(exists(
        app._tempdir.path(),
        &format!("cache/inputs/{sha}.jpg")
    ));
}

#[tokio::test]
async fn dry_run_leaves_disk_and_db_untouched() {
    let app = common::test_app().await;
    let now = chrono::Utc::now().timestamp();

    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), "d".repeat(64).as_str(), None).await;
    common::seed_job(
        &app.db,
        "job-3",
        "done",
        None,
        None,
        "flux2_klein_edit",
        input_id,
        now - 40 * DAY,
        Some(now - 40 * DAY),
    )
    .await;
    mark_job_deleted_with_paths(
        &app.db,
        "job-3",
        now - 40 * DAY,
        "outputs/job-3.jpg",
        "thumbs/job-3.jpg",
        "previews/job-3.jpg",
    )
    .await;
    write(app._tempdir.path(), "outputs/job-3.jpg");

    let opts = PurgeOpts {
        now_seconds: now,
        dry_run: true,
        ..PurgeOpts::defaults_now()
    };
    let report = purge::run(&app.state, opts).await.unwrap();

    assert_eq!(
        report.jobs_hard_deleted, 1,
        "report still reflects what WOULD happen"
    );
    assert!(
        exists(app._tempdir.path(), "outputs/job-3.jpg"),
        "dry_run must not delete files"
    );

    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM jobs WHERE id = ?")
        .bind("job-3")
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(row.is_some(), "dry_run must not delete the row");
}

/// Write a file into a fake ComfyUI data dir and backdate its mtime.
fn write_comfy_file(comfy_dir: &std::path::Path, sub: &str, name: &str, age_days: u64) {
    let abs = comfy_dir.join(sub).join(name);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, b"x").unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_days * 86400);
    std::fs::File::options()
        .write(true)
        .open(&abs)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[tokio::test]
async fn purge_sweeps_only_stale_zun_files_from_comfy_dirs() {
    // ComfyUI never prunes input/ or output/ and has no delete endpoint, so
    // the purge task cleans up the files this server put there. It must
    // touch nothing else in those directories.
    let mut app = common::test_app().await;
    let comfy = app._tempdir.path().join("comfy");
    app.state.config.comfy_data_dir = Some(comfy.clone());

    write_comfy_file(&comfy, "input", "zun_abc.jpg", 90); // ours, stale
    write_comfy_file(&comfy, "output", "zun_abc_00001_.png", 90); // ours, stale
    write_comfy_file(&comfy, "input", "zun_fresh.jpg", 1); // ours, still recent
    write_comfy_file(&comfy, "input", "vacation.jpg", 90); // someone else's
    write_comfy_file(&comfy, "output", "ComfyUI_00042_.png", 90); // someone else's

    let report = purge::run(&app.state, PurgeOpts::for_retention_days_now(30))
        .await
        .unwrap();
    assert_eq!(
        report.comfy_files_removed, 2,
        "only the two stale zun_ files"
    );

    let gone = |sub: &str, n: &str| !comfy.join(sub).join(n).exists();
    assert!(gone("input", "zun_abc.jpg"));
    assert!(gone("output", "zun_abc_00001_.png"));
    assert!(
        !gone("input", "zun_fresh.jpg"),
        "inside retention, must stay"
    );
    assert!(
        !gone("input", "vacation.jpg"),
        "not ours, must never be touched"
    );
    assert!(
        !gone("output", "ComfyUI_00042_.png"),
        "not ours, must never be touched"
    );
}

#[tokio::test]
async fn purge_leaves_comfy_dirs_alone_when_unconfigured() {
    let app = common::test_app().await;
    assert!(app.state.config.comfy_data_dir.is_none(), "off by default");
    let report = purge::run(&app.state, PurgeOpts::for_retention_days_now(30))
        .await
        .unwrap();
    assert_eq!(report.comfy_files_removed, 0);
}

#[tokio::test]
async fn purge_dry_run_counts_comfy_files_without_deleting() {
    let mut app = common::test_app().await;
    let comfy = app._tempdir.path().join("comfy");
    app.state.config.comfy_data_dir = Some(comfy.clone());
    write_comfy_file(&comfy, "input", "zun_abc.jpg", 90);

    let opts = PurgeOpts {
        dry_run: true,
        ..PurgeOpts::for_retention_days_now(30)
    };
    let report = purge::run(&app.state, opts).await.unwrap();
    assert_eq!(report.comfy_files_removed, 1);
    assert!(comfy.join("input").join("zun_abc.jpg").exists());
}
