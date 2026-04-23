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

/// Drop `input_path`, `output_path`, or a `thumb_path` file under the
/// tempdir so the image handlers have bytes to serve.
async fn write_relative(tempdir: &std::path::Path, rel: &str, bytes: &[u8]) {
    let abs = tempdir.join(rel);
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(abs, bytes).await.unwrap();
}

// ---------- /api/jobs/{id}/input ----------

#[tokio::test]
async fn get_input_serves_bytes_with_image_content_type() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "job-a",
        "queued",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    write_relative(app._tempdir.path(), "inputs/job-a.jpg", b"fake-jpeg-bytes").await;

    let resp = app
        .router
        .oneshot(authed("/api/jobs/job-a/input"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    assert!(
        resp.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("max-age=3600")
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes, b"fake-jpeg-bytes".as_ref());
}

#[tokio::test]
async fn get_input_unknown_job_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("/api/jobs/does-not-exist/input"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_input_missing_file_is_404() {
    // Row exists but the input file was never written.
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "job-b",
        "queued",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("/api/jobs/job-b/input"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------- /api/jobs/{id}/result ----------

#[tokio::test]
async fn get_result_requires_done_status() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "queued-job",
        "queued",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("/api/jobs/queued-job/result"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "not_ready");
}

#[tokio::test]
async fn get_result_serves_png_when_done() {
    let app = common::test_app().await;
    let png = common::tiny_png(16, 16);
    common::seed_job(
        &app.db,
        "done-job",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_030),
    )
    .await;
    sqlx::query("UPDATE jobs SET output_path = ? WHERE id = ?")
        .bind("outputs/zun_done-job_00001_.png")
        .bind("done-job")
        .execute(&app.db)
        .await
        .unwrap();
    write_relative(app._tempdir.path(), "outputs/zun_done-job_00001_.png", &png).await;

    let resp = app
        .router
        .oneshot(authed("/api/jobs/done-job/result"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes, png.as_slice());
}

// ---------- /api/jobs/{id}/thumb ----------

#[tokio::test]
async fn get_thumb_lazy_generates_jpeg_and_caches() {
    let app = common::test_app().await;
    let png = common::tiny_png(600, 400); // non-square, > 400 on longest side
    common::seed_job(
        &app.db,
        "thumb-job",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_030),
    )
    .await;
    sqlx::query("UPDATE jobs SET output_path = ? WHERE id = ?")
        .bind("outputs/zun_thumb-job_00001_.png")
        .bind("thumb-job")
        .execute(&app.db)
        .await
        .unwrap();
    write_relative(
        app._tempdir.path(),
        "outputs/zun_thumb-job_00001_.png",
        &png,
    )
    .await;

    let router = app.router.clone();

    // First call: no cached thumb → generates one.
    let resp = router
        .clone()
        .oneshot(authed("/api/jobs/thumb-job/thumb"))
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
    // Decode the returned bytes to confirm they're a valid JPEG and the
    // longest side is <= 400. (Size comparison vs source is skipped:
    // synthetic test PNGs compress extremely well, so the resulting JPEG
    // can actually be larger for trivial patterns — not meaningful.)
    let reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .unwrap();
    let (w, h) = reader.into_dimensions().unwrap();
    assert!(
        w.max(h) <= 400,
        "thumb longest side should be ≤ 400, got {w}x{h}"
    );

    // Thumb file was cached on disk.
    let cached_abs = app._tempdir.path().join("thumbs/thumb-job.jpg");
    assert!(cached_abs.exists());

    // DB thumb_path populated.
    let (thumb_path,): (String,) = sqlx::query_as("SELECT thumb_path FROM jobs WHERE id = ?")
        .bind("thumb-job")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(thumb_path, "thumbs/thumb-job.jpg");

    // Second call: should hit the cached file (same bytes).
    let resp2 = router
        .oneshot(authed("/api/jobs/thumb-job/thumb"))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = resp2
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    assert_eq!(bytes, bytes2);
}

#[tokio::test]
async fn get_thumb_requires_done_status() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "running-job",
        "running",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("/api/jobs/running-job/thumb"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn get_input_sends_etag_and_honors_if_none_match() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "etag-job",
        "queued",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    write_relative(
        app._tempdir.path(),
        "inputs/etag-job.jpg",
        b"fake-jpeg-bytes",
    )
    .await;

    // First request: no If-None-Match → 200 + ETag header.
    let router = app.router.clone();
    let resp = router
        .clone()
        .oneshot(authed("/api/jobs/etag-job/input"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get("etag")
        .expect("etag header present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(etag.starts_with('\"') && etag.ends_with('\"'));

    // Second request with matching If-None-Match → 304, empty body.
    let req = Request::builder()
        .method("GET")
        .uri("/api/jobs/etag-job/input")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("if-none-match", &etag)
        .body(Body::empty())
        .unwrap();
    let resp2 = router.oneshot(req).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(resp2.headers().get("etag").unwrap().to_str().unwrap(), etag);
    let body = resp2.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test]
async fn image_endpoints_require_auth() {
    let app = common::test_app().await;
    for uri in [
        "/api/jobs/x/input",
        "/api/jobs/x/result",
        "/api/jobs/x/thumb",
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
