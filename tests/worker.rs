//! End-to-end worker tests: spin up a wiremock server impersonating ComfyUI,
//! submit a job via the HTTP API, and watch the row progress queued→done.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path as mock_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn authed_post_submit(ct: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap()
}

fn authed_get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

async fn wait_for_status(
    router: &axum::Router,
    job_id: &str,
    target: &str,
    max: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        let resp = router
            .clone()
            .oneshot(authed_get(&format!("/api/v1/jobs/{job_id}")))
            .await
            .unwrap();
        let body = body_json(resp).await;
        let status = body["status"].as_str().unwrap_or("");
        if status == target {
            return body;
        }
        if status == "failed" && target != "failed" {
            panic!("job failed unexpectedly: {body}");
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("job {job_id} did not reach status={target} within {max:?}; last body: {body}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn minimal_workflow() -> serde_json::Value {
    json!({
        "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" }, "class_type": "LoadImage" },
        "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" }, "class_type": "CLIPTextEncode" },
        "16": { "inputs": { "noise_seed": "SEED_PLACEHOLDER" }, "class_type": "RandomNoise" },
        "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" }, "class_type": "SaveImage" }
    })
}

async fn seed_test_prompt(app: &mut common::TestApp) -> i64 {
    common::seed_workflow(app, "flux2_klein_edit", minimal_workflow());
    common::seed_prompt(&app.db, "Test", "test prompt", "flux2_klein_edit").await
}

#[tokio::test]
async fn submit_to_done_roundtrip_via_worker() {
    let comfy = MockServer::start().await;

    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "name": "zun_upload.jpg", "subfolder": "", "type": "input" }),
            ),
        )
        .mount(&comfy)
        .await;

    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .and(body_string_contains("test prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "prompt_id": "fake-prompt-xyz",
            "number": 1,
            "node_errors": {}
        })))
        .mount(&comfy)
        .await;

    let png = common::tiny_png(32, 24);
    Mock::given(method("GET"))
        .and(mock_path("/view"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.clone()))
        .mount(&comfy)
        .await;

    let ws_url = common::start_ws_mock(vec![common::ws_success_frame("fake-prompt-xyz")]).await;

    let mut app = common::test_app_with_comfy_and_ws(&comfy.uri(), &ws_url).await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let router = app.router.clone();

    let img = b"fake-jpeg-bytes";
    let (ct, body) = common::multipart_submit(img, "image/jpeg", Some(prompt_id), None, None);
    let resp = router
        .clone()
        .oneshot(authed_post_submit(&ct, body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let job_id = body_json(resp).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    sqlx::query("UPDATE custom_prompts SET text = ? WHERE id = ?")
        .bind("edited after submit")
        .bind(prompt_id)
        .execute(&app.db)
        .await
        .unwrap();

    Mock::given(method("GET"))
        .and(mock_path("/history/fake-prompt-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "fake-prompt-xyz": {
                "status": { "completed": true, "status_str": "success", "messages": [] },
                "outputs": {
                    "19": { "images": [
                        { "filename": format!("zun_{job_id}_00001_.png"), "subfolder": "", "type": "output" }
                    ] }
                }
            }
        })))
        .mount(&comfy)
        .await;

    let _handle = common::spawn_worker(&mut app);

    let done = wait_for_status(&router, &job_id, "done", Duration::from_secs(10)).await;
    assert_eq!(done["status"], "done");
    assert!(done["completed_at"].as_i64().unwrap() > 0);

    let output_rel = format!("outputs/zun_{job_id}_00001_.png");
    let output_abs = app._tempdir.path().join(&output_rel);
    assert!(output_abs.exists(), "output should be at {output_abs:?}");
    let bytes = std::fs::read(&output_abs).unwrap();
    assert_eq!(bytes, png);

    assert_eq!(done["width"], 32);
    assert_eq!(done["height"], 24);
}

#[tokio::test]
async fn worker_marks_failed_on_comfy_execution_error() {
    let comfy = MockServer::start().await;

    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "name": "zun_x.jpg" })))
        .mount(&comfy)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "prompt_id": "bad-prompt" })),
        )
        .mount(&comfy)
        .await;

    let ws_url = common::start_ws_mock(vec![
        r#"{"type":"execution_error","data":{"prompt_id":"bad-prompt","exception_message":"boom"}}"#
            .to_string(),
    ])
    .await;

    let mut app = common::test_app_with_comfy_and_ws(&comfy.uri(), &ws_url).await;
    let prompt_id = seed_test_prompt(&mut app).await;
    let router = app.router.clone();

    let (ct, body) = common::multipart_submit(b"xxx", "image/jpeg", Some(prompt_id), None, None);
    let resp = router
        .clone()
        .oneshot(authed_post_submit(&ct, body))
        .await
        .unwrap();
    let job_id = body_json(resp).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    let _handle = common::spawn_worker(&mut app);

    let failed = wait_for_status(&router, &job_id, "failed", Duration::from_secs(10)).await;
    assert_eq!(failed["status"], "failed");
    let err = failed["error"].as_str().unwrap_or("");
    assert!(
        err.contains("execution_error"),
        "unexpected error message: {err}"
    );
}

#[tokio::test]
async fn worker_exits_cleanly_when_idle_on_shutdown() {
    let comfy = MockServer::start().await;
    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    let (handle, shutdown_tx) = common::spawn_worker_with_shutdown(&mut app);

    shutdown_tx.send(true).expect("send shutdown");

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("worker did not exit within 2s of shutdown signal")
        .expect("worker task panicked");
}

#[tokio::test]
async fn worker_skips_soft_deleted_queued_jobs() {
    // A queued job that was soft-deleted before the worker drained must not
    // run on the GPU — pre-fix, fetch_oldest_queued ignored deleted_at.
    let comfy = MockServer::start().await;
    // Mount loud-failing mocks: if the worker DOES try to run this job,
    // an /upload/image call would 500 and the test would observe `failed`.
    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&comfy)
        .await;

    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"b".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "soft-deleted",
        "queued",
        None,
        Some("test prompt"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        None,
    )
    .await;
    sqlx::query("UPDATE jobs SET deleted_at = ? WHERE id = ?")
        .bind(1_700_000_001_i64)
        .bind("soft-deleted")
        .execute(&app.db)
        .await
        .unwrap();

    let _handle = common::spawn_worker(&mut app);

    // Give the worker a chance to pick it up. After 500ms it should still
    // be queued; if the bug regresses, status would flip to running/failed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, started_at): (String, Option<i64>) =
        sqlx::query_as("SELECT status, started_at FROM jobs WHERE id = ?")
            .bind("soft-deleted")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(status, "queued", "soft-deleted job must not be picked up");
    assert!(
        started_at.is_none(),
        "soft-deleted job must never enter running",
    );
}

#[tokio::test]
async fn worker_resets_running_jobs_to_queued_on_startup() {
    let comfy = MockServer::start().await;
    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"x")).await;
    common::seed_job(
        &app.db,
        "stranded",
        "running",
        None,
        Some("test prompt"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        None,
    )
    .await;

    let _handle = common::spawn_worker(&mut app);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let (status,): (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
            .bind("stranded")
            .fetch_one(&app.db)
            .await
            .unwrap();
        if status != "running" {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("stranded running job stayed in `running`; reset did not fire");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn worker_interrupts_comfy_when_a_job_times_out() {
    // Our per-job timeout does not stop ComfyUI on its own: the prompt keeps
    // running and holding the GPU, and the next job silently queues behind it
    // inside ComfyUI. The worker must interrupt explicitly.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "name": "in.jpg" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "prompt_id": "stuck-1" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/interrupt"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // A ws that completes the upgrade and then never says anything, so the
    // job can only end by hitting its timeout.
    let ws_url = common::start_ws_mock(vec![]).await;
    let mut app = common::test_app_with_comfy_and_ws(&server.uri(), &ws_url).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());

    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"c".repeat(64), Some(b"img")).await;
    sqlx::query(
        "INSERT INTO jobs (id, input_id, source_prompt_id, prompt_text, workflow, \
         timeout_seconds, seed, status, created_at) \
         VALUES ('to1', ?, NULL, 'p', 'flux2_klein_edit', 1, 0, 'queued', 100)",
    )
    .bind(input_id)
    .execute(&app.db)
    .await
    .unwrap();

    let _worker = common::spawn_worker(&mut app);
    let body = wait_for_status(&app.router, "to1", "failed", Duration::from_secs(15)).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("timeout"),
        "expected a timeout error, got: {body}"
    );

    let interrupts = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == "/interrupt")
        .count();
    assert_eq!(interrupts, 1, "timed-out job must interrupt ComfyUI");
}

#[tokio::test]
async fn worker_clamps_a_nonsense_stored_timeout_instead_of_hanging_forever() {
    // Rows written before `timeout_seconds` was range-checked (or edited
    // straight in SQLite) can still hold a negative value. Read back as
    // `-1i64 as u64` that is u64::MAX — a timeout that never fires, on a
    // worker that runs one job at a time. The read path clamps instead, so
    // the job fails on its own and the queue keeps moving.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "name": "in.jpg" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "prompt_id": "neg-1" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/interrupt"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let ws_url = common::start_ws_mock(vec![]).await;
    let mut app = common::test_app_with_comfy_and_ws(&server.uri(), &ws_url).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());

    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"d".repeat(64), Some(b"img")).await;
    sqlx::query(
        "INSERT INTO jobs (id, input_id, source_prompt_id, prompt_text, workflow, \
         timeout_seconds, seed, status, created_at) \
         VALUES ('neg1', ?, NULL, 'p', 'flux2_klein_edit', -1, 0, 'queued', 100)",
    )
    .bind(input_id)
    .execute(&app.db)
    .await
    .unwrap();

    let _worker = common::spawn_worker(&mut app);
    let body = wait_for_status(&app.router, "neg1", "failed", Duration::from_secs(15)).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("timeout"),
        "expected a timeout error, got: {body}"
    );
}

#[tokio::test]
async fn stored_job_error_is_redacted_like_a_5xx_body() {
    // `GET /jobs/{id}` hands `error` straight to the phone, but it used to
    // be stored verbatim — so a failed job leaked the configured comfy_url
    // and any data_dir path in the chain, the exact things AppError strips
    // from a 5xx body. The operator still gets the raw chain in the
    // `job.failed` audit line.
    //
    // Uses a reachable-but-broken backend on purpose: an unreachable one is
    // now requeued rather than failed, so it would never store a message.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let mut app = common::test_app_with_comfy(&server.uri()).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"e".repeat(64), Some(b"img")).await;
    common::seed_job(
        &app.db,
        "red1",
        "queued",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        100,
        None,
    )
    .await;

    let _worker = common::spawn_worker(&mut app);
    let body = wait_for_status(&app.router, "red1", "failed", Duration::from_secs(30)).await;
    let err = body["error"].as_str().unwrap_or_default();

    assert!(err.contains("<url>"), "url should be redacted, got: {err}");
    let host = server.uri().replace("http://", "");
    assert!(!err.contains(&host), "leaked the comfy url: {err}");
    assert!(
        err.contains("500") || err.to_lowercase().contains("status"),
        "redaction must keep the useful part: {err}"
    );
}

#[tokio::test]
async fn unreachable_backend_requeues_the_job_instead_of_failing_it() {
    // A ComfyUI restart used to take the whole queue with it: every queued
    // job got claimed, burned its retries, and was marked `failed` ~1.5 s
    // apart, with no way to get any of them back. "The backend isn't there"
    // is not the job's fault, so the row goes back on the queue.
    let mut app = common::test_app_with_comfy("http://127.0.0.1:1").await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"f".repeat(64), Some(b"img")).await;
    for id in ["rq1", "rq2"] {
        common::seed_job(
            &app.db,
            id,
            "queued",
            None,
            Some("p"),
            "flux2_klein_edit",
            input_id,
            100,
            None,
        )
        .await;
    }

    let _worker = common::spawn_worker(&mut app);
    // Give the worker time to claim, fail to connect, and put the row back.
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
            .fetch_all(&app.db)
            .await
            .unwrap();
        assert!(
            !statuses.iter().any(|s| s == "failed"),
            "a down backend must not fail queued jobs, got {statuses:?}",
        );
    }
    let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
        .fetch_all(&app.db)
        .await
        .unwrap();
    assert!(
        statuses.iter().all(|s| s == "queued"),
        "both jobs should still be queued, got {statuses:?}",
    );
}
