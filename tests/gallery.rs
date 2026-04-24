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

fn authed(method: &'static str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

// ---------- GET /api/jobs ----------

#[tokio::test]
async fn list_empty_returns_empty_array() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_default_status_is_done_only() {
    let app = common::test_app().await;
    // Three jobs across three statuses.
    common::seed_job(
        &app.db,
        "j-done",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_020),
    )
    .await;
    common::seed_job(
        &app.db,
        "j-queued",
        "queued",
        "test_prompt",
        1_700_000_100,
        None,
    )
    .await;
    common::seed_job(
        &app.db,
        "j-failed",
        "failed",
        "test_prompt",
        1_700_000_200,
        Some(1_700_000_250),
    )
    .await;

    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "default status filter should return only done jobs"
    );
    assert_eq!(items[0]["id"], "j-done");
}

#[tokio::test]
async fn list_respects_status_query() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "j-q1",
        "queued",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;
    common::seed_job(
        &app.db,
        "j-q2",
        "queued",
        "test_prompt",
        1_700_000_100,
        None,
    )
    .await;
    common::seed_job(
        &app.db,
        "j-d1",
        "done",
        "test_prompt",
        1_700_000_050,
        Some(1_700_000_060),
    )
    .await;

    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs?status=queued"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest first.
    assert_eq!(items[0]["id"], "j-q2");
    assert_eq!(items[1]["id"], "j-q1");
}

#[tokio::test]
async fn list_includes_prompt_label_and_duration() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "j1",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_024),
    )
    .await;
    // Unknown prompt_id (e.g., prompt was since removed from catalog).
    common::seed_job(
        &app.db,
        "j2",
        "done",
        "gone_prompt",
        1_700_000_500,
        Some(1_700_000_530),
    )
    .await;

    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);

    // Newest first: j2 then j1.
    assert_eq!(items[0]["id"], "j2");
    assert_eq!(items[0]["prompt_id"], "gone_prompt");
    assert!(
        items[0]["prompt_label"].is_null(),
        "unknown prompt => null label"
    );
    assert_eq!(items[0]["duration_seconds"], 30);

    assert_eq!(items[1]["id"], "j1");
    assert_eq!(items[1]["prompt_label"], "Test Prompt");
    assert_eq!(items[1]["duration_seconds"], 24);
}

#[tokio::test]
async fn list_duration_is_null_without_completed_at() {
    let app = common::test_app().await;
    common::seed_job(&app.db, "j", "done", "test_prompt", 1_700_000_000, None).await;

    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert!(items[0]["duration_seconds"].is_null());
}

#[tokio::test]
async fn list_limit_param_caps_results() {
    let app = common::test_app().await;
    for i in 0..5 {
        let id = format!("j{i}");
        common::seed_job(
            &app.db,
            &id,
            "done",
            "test_prompt",
            1_700_000_000 + i,
            Some(1_700_000_100),
        )
        .await;
    }
    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs?limit=2"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest first: j4, j3.
    assert_eq!(items[0]["id"], "j4");
    assert_eq!(items[1]["id"], "j3");
}

#[tokio::test]
async fn list_before_cursor_paginates() {
    let app = common::test_app().await;
    for i in 0..5 {
        let id = format!("j{i}");
        common::seed_job(
            &app.db,
            &id,
            "done",
            "test_prompt",
            1_700_000_000 + i,
            Some(1_700_000_100),
        )
        .await;
    }
    // Return rows with created_at < 1_700_000_003 → j0, j1, j2.
    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs?before=1700000003"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["id"], "j2");
    assert_eq!(items[2]["id"], "j0");
}

#[tokio::test]
async fn list_emits_link_header_when_page_is_full() {
    let app = common::test_app().await;
    for i in 0..5 {
        let id = format!("j{i}");
        common::seed_job(
            &app.db,
            &id,
            "done",
            "test_prompt",
            1_700_000_000 + i,
            Some(1_700_000_100),
        )
        .await;
    }

    // limit=3 over 5 rows → full page → Link header present, pointing at
    // the oldest row's created_at.
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/jobs?limit=3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let link = resp
        .headers()
        .get("link")
        .expect("Link header on full page")
        .to_str()
        .unwrap()
        .to_string();
    assert!(link.contains("rel=\"next\""), "got: {link}");
    assert!(link.contains("before=1700000002"), "got: {link}");
    assert!(link.contains("limit=3"), "got: {link}");

    // limit=10 over 5 rows → partial page → no Link header.
    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs?limit=10"))
        .await
        .unwrap();
    assert!(resp.headers().get("link").is_none());
}

#[tokio::test]
async fn list_limit_is_clamped_to_100() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("GET", "/api/jobs?limit=999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Smoke: shouldn't reject, just cap.
    let _body = body_json(resp).await;
}

#[tokio::test]
async fn list_requires_auth() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------- DELETE /api/jobs/{id} ----------

#[tokio::test]
async fn delete_unknown_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("DELETE", "/api/jobs/nope-nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_after_submit_removes_row_and_file() {
    let app = common::test_app().await;
    let router = app.router.clone();
    let tmpdir_path = app._tempdir.path().to_path_buf();

    // Submit a real job so the input file actually exists.
    let (ct, body) =
        common::multipart_image_job(b"fake-jpeg-bytes", "image/jpeg", common::KNOWN_PROMPT_ID);
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("authorization", common::bearer(common::TEST_TOKEN))
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let created = body_json(resp).await;
    let job_id = created["job_id"].as_str().unwrap().to_string();
    let input_path = tmpdir_path.join(format!("inputs/{job_id}.jpg"));
    assert!(input_path.exists(), "input file should exist before delete");

    // Delete.
    let resp = router
        .clone()
        .oneshot(authed("DELETE", &format!("/api/jobs/{job_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(!input_path.exists(), "input file should be gone");

    // Row also gone.
    let resp = router
        .oneshot(authed("GET", &format!("/api/jobs/{job_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_is_idempotent_from_404_onward() {
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "once-only",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("DELETE", "/api/jobs/once-only"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .oneshot(authed("DELETE", "/api/jobs/once-only"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_tolerates_missing_input_file() {
    // seed_job records input_path but doesn't actually write the file.
    let app = common::test_app().await;
    common::seed_job(
        &app.db,
        "no-file",
        "done",
        "test_prompt",
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("DELETE", "/api/jobs/no-file"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_requires_auth() {
    let app = common::test_app().await;
    common::seed_job(&app.db, "keep", "done", "test_prompt", 1_700_000_000, None).await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/jobs/keep")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
