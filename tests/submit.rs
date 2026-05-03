use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

mod common;

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn authed_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

fn submit_request(content_type: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", content_type)
        .body(Body::from(body))
        .unwrap()
}

async fn seed_test_prompt(app: &mut common::TestApp) -> i64 {
    common::seed_workflow(app, "flux2_klein_edit", common::supported_edit_workflow());
    common::seed_prompt(&app.db, "Test", "test prompt", "flux2_klein_edit").await
}

#[tokio::test]
async fn submit_valid_job_returns_202_with_location_and_job_id() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let router = app.router.clone();
    let img = b"fake-jpeg-bytes";
    let (ct, body) = common::multipart_submit(img, "image/jpeg", Some(prompt_id), None, None);

    let resp = router
        .clone()
        .oneshot(submit_request(&ct, body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let location = resp
        .headers()
        .get("location")
        .expect("Location header present")
        .to_str()
        .unwrap()
        .to_string();
    let created = body_json(resp).await;
    let job_id = created["job_id"].as_str().unwrap().to_string();
    let input_id = created["input_id"].as_i64().unwrap();
    assert_eq!(job_id.len(), 36);
    assert_eq!(location, format!("/api/v1/jobs/{job_id}"));
    assert!(input_id > 0);

    // Cache file landed under data/cache/inputs/<sha>.jpg
    let sha = zun_rust_server::hash::sha256_hex(img);
    let expected = app._tempdir.path().join(format!("cache/inputs/{sha}.jpg"));
    assert!(expected.exists(), "expected cache at {expected:?}");

    // Job is queryable, queued, has a non-zero seed.
    let resp = router
        .oneshot(authed_get(&format!("/api/v1/jobs/{job_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["id"], job_id);
    assert_eq!(status["status"], "queued");
    assert_eq!(status["source_prompt_id"], prompt_id);
    assert_eq!(status["prompt_text"], "test prompt");
    assert_eq!(status["workflow"], "flux2_klein_edit");
    assert_eq!(status["input_id"], input_id);
    assert!(status["seed"].as_i64().is_some());
}

#[tokio::test]
async fn submit_with_known_hash_via_json_skips_upload() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let img = b"fake-jpeg-bytes";
    let sha = zun_rust_server::hash::sha256_hex(img);
    // Pre-seed the input as if a previous upload happened.
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(img)).await;

    let body = serde_json::json!({
        "input_sha256": sha,
        "prompt_id": prompt_id,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let created = body_json(resp).await;
    assert_eq!(created["input_id"], input_id, "should reuse existing input");
}

#[tokio::test]
async fn submit_json_with_known_hash_but_missing_file_returns_409_and_clears_path() {
    // Ground truth: an inputs row claims to have a cached file, but the
    // file is gone from disk (manual cleanup, partial restore, etc.).
    // The submit handler must surface NeedUpload AND null out the stale
    // path so subsequent hash-only submits also fail fast.
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let img = b"orphan-bytes";
    let sha = zun_rust_server::hash::sha256_hex(img);
    let input_id = common::seed_input(&app.db, app._tempdir.path(), &sha, Some(img)).await;

    // Delete the file out from under the row.
    let abs = app._tempdir.path().join(format!("cache/inputs/{sha}.jpg"));
    std::fs::remove_file(&abs).expect("remove cached file");

    let body = serde_json::json!({
        "input_sha256": sha,
        "prompt_id": prompt_id,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "need_upload");
    assert_eq!(body["input_id"], input_id);

    // The handler should have cleared the stale path so the row reflects
    // disk reality.
    let path: Option<String> = sqlx::query_scalar("SELECT path FROM inputs WHERE id = ?")
        .bind(input_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(
        path.is_none(),
        "expected NULL path after disk-miss; got {path:?}"
    );
}

#[tokio::test]
async fn submit_json_with_unknown_hash_returns_409_need_upload() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let unknown_sha = "0".repeat(64);
    let body = serde_json::json!({
        "input_sha256": unknown_sha,
        "prompt_id": prompt_id,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "need_upload");
    assert_eq!(body["need_upload"], true);
}

#[tokio::test]
async fn submit_unknown_prompt_id_is_400() {
    let app = common::test_app().await;
    let img = b"x";
    let (ct, body) = common::multipart_submit(img, "image/jpeg", Some(9999), None, None);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_missing_image_is_400() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let (ct, body) = common::multipart_no_image(prompt_id);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_unsupported_content_type_is_400() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let img = b"x";
    let (ct, body) = common::multipart_submit(img, "image/gif", Some(prompt_id), None, None);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_requires_auth() {
    let mut app = common::test_app().await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let (ct, body) = common::multipart_submit(b"x", "image/jpeg", Some(prompt_id), None, None);
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/jobs")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn submit_with_prompt_text_requires_workflow() {
    let app = common::test_app().await;
    let img = b"x";
    let (ct, body) = common::multipart_submit(img, "image/jpeg", None, Some("free text"), None);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_prompt_text_with_workflow_works() {
    let mut app = common::test_app().await;
    common::seed_workflow(
        &mut app,
        "flux2_klein_edit",
        common::supported_edit_workflow(),
    );
    let img = b"yyy";
    let (ct, body) = common::multipart_submit(
        img,
        "image/jpeg",
        None,
        Some("free text"),
        Some("flux2_klein_edit"),
    );
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn submit_prompt_text_with_9b_kv_experimental_model_works() {
    let app = common::test_app().await;
    let img = b"yyy";
    let (ct, body) = common::multipart_submit(
        img,
        "image/jpeg",
        None,
        Some("free text"),
        Some("flux2_klein_9b_kv_experimental"),
    );
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn get_unknown_job_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed_get(
            "/api/v1/jobs/00000000-0000-0000-0000-000000000000",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
