use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use std::io::Cursor;
use tower::ServiceExt;

mod common;

fn authed(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

async fn write_relative(tempdir: &std::path::Path, rel: &str, bytes: &[u8]) {
    let abs = tempdir.join(rel);
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(abs, bytes).await.unwrap();
}

// ---------- /api/v1/inputs/{id}/file ----------

#[tokio::test]
async fn get_input_file_serves_bytes_with_image_content_type() {
    let app = common::test_app().await;
    let sha = "a".repeat(64);
    let bytes = b"fake-jpeg-bytes";
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(bytes)).await;

    let resp = app
        .router
        .oneshot(authed(&format!("/api/v1/inputs/{input_id}/file")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, bytes.as_ref());
}

#[tokio::test]
async fn get_input_file_unknown_input_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("/api/v1/inputs/99999/file"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_input_metadata_reports_available_flag() {
    let app = common::test_app().await;
    let sha = "b".repeat(64);
    let bytes = b"present";
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(bytes)).await;

    let resp = app
        .router
        .clone()
        .oneshot(authed(&format!("/api/v1/inputs/{input_id}")))
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["available"], true);

    // Manually null out path to simulate purge.
    sqlx::query("UPDATE inputs SET path = NULL WHERE id = ?")
        .bind(input_id)
        .execute(&app.db)
        .await
        .unwrap();
    let resp = app
        .router
        .oneshot(authed(&format!("/api/v1/inputs/{input_id}")))
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["available"], false);
}

// ---------- /api/v1/jobs/{id}/result ----------

#[tokio::test]
async fn get_result_requires_done_status() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "queued-job",
        "queued",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        None,
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("/api/v1/jobs/queued-job/result"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn get_result_serves_png_when_done() {
    let app = common::test_app().await;
    let png = common::tiny_png(16, 16);
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "done-job",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_030),
    )
    .await;
    let rel = "outputs/zun_done-job_00001_.png";
    sqlx::query("UPDATE jobs SET output_path = ? WHERE id = ?")
        .bind(rel)
        .bind("done-job")
        .execute(&app.db)
        .await
        .unwrap();
    write_relative(app._tempdir.path(), rel, &png).await;

    let resp = app
        .router
        .oneshot(authed("/api/v1/jobs/done-job/result"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes, png.as_slice());
}

// ---------- /api/v1/jobs/{id}/thumb ----------

#[tokio::test]
async fn get_thumb_lazy_generates_jpeg_and_caches_under_thumb_dir() {
    let app = common::test_app().await;
    let png = common::tiny_png(600, 400);
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"c".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "thumb-job",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_030),
    )
    .await;
    let out_rel = "outputs/zun_thumb-job_00001_.png";
    sqlx::query("UPDATE jobs SET output_path = ? WHERE id = ?")
        .bind(out_rel)
        .bind("thumb-job")
        .execute(&app.db)
        .await
        .unwrap();
    write_relative(app._tempdir.path(), out_rel, &png).await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("/api/v1/jobs/thumb-job/thumb"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .unwrap();
    let (w, h) = reader.into_dimensions().unwrap();
    assert!(w.max(h) <= 400);

    // Cached under the thumbs dir.
    let cached_abs = app._tempdir.path().join("thumbs/thumb-job.jpg");
    assert!(
        cached_abs.exists(),
        "expected cached thumb at {cached_abs:?}"
    );

    let (thumb_path,): (String,) = sqlx::query_as("SELECT thumb_path FROM jobs WHERE id = ?")
        .bind("thumb-job")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(thumb_path, "thumbs/thumb-job.jpg");
}

#[tokio::test]
async fn get_preview_lazy_generates_jpeg_and_caches() {
    let app = common::test_app().await;
    let png = common::tiny_png(2000, 1500);
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"d".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "preview-job",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_030),
    )
    .await;
    let out_rel = "outputs/zun_preview-job_00001_.png";
    sqlx::query("UPDATE jobs SET output_path = ? WHERE id = ?")
        .bind(out_rel)
        .bind("preview-job")
        .execute(&app.db)
        .await
        .unwrap();
    write_relative(app._tempdir.path(), out_rel, &png).await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("/api/v1/jobs/preview-job/preview"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .unwrap();
    let (w, h) = reader.into_dimensions().unwrap();
    assert!(w.max(h) <= 1280, "preview longest side ≤1280, got {w}x{h}");
    // Preview is markedly larger than a 400px thumb.
    assert!(w.max(h) > 400);

    let cached = app._tempdir.path().join("previews/preview-job.jpg");
    assert!(cached.exists(), "expected preview cached at {cached:?}");

    let (preview_path,): (String,) = sqlx::query_as("SELECT preview_path FROM jobs WHERE id = ?")
        .bind("preview-job")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(preview_path, "previews/preview-job.jpg");
}

#[tokio::test]
async fn image_endpoints_require_auth() {
    let app = common::test_app().await;
    for uri in [
        "/api/v1/inputs/1/file",
        "/api/v1/jobs/x/result",
        "/api/v1/jobs/x/thumb",
        "/api/v1/jobs/x/preview",
    ] {
        let resp = app
            .router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "uri={uri}");
    }
}
