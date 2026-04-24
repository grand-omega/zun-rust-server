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
use wiremock::matchers::{method, path as mock_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn authed_post_submit(ct: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/jobs")
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

/// Wait until a job's status reaches `target` or the deadline expires.
/// Returns the final status JSON; panics on timeout.
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
            .oneshot(authed_get(&format!("/api/jobs/{job_id}")))
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
    // Mirrors the shape of flux2_klein_edit well enough for build_edit_workflow
    // to substitute all three placeholders.
    json!({
        "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" }, "class_type": "LoadImage" },
        "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" }, "class_type": "CLIPTextEncode" },
        "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" }, "class_type": "SaveImage" }
    })
}

#[tokio::test]
async fn submit_to_done_roundtrip_via_worker() {
    // Stand up a fake ComfyUI.
    let comfy = MockServer::start().await;

    // /upload/image echoes back a stored name.
    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "name": "zun_upload.jpg", "subfolder": "", "type": "input" }),
            ),
        )
        .mount(&comfy)
        .await;

    // /prompt returns a known prompt id.
    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "prompt_id": "fake-prompt-xyz",
            "number": 1,
            "node_errors": {}
        })))
        .mount(&comfy)
        .await;

    // /view returns a real (tiny) PNG so the worker's dimension
    // extraction has something valid to decode.
    let png = common::tiny_png(32, 24);
    Mock::given(method("GET"))
        .and(mock_path("/view"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.clone()))
        .mount(&comfy)
        .await;

    // Build the test app and wire it to the fake ComfyUI.
    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());
    let router = app.router.clone();

    // Submit a job.
    let (ct, body) =
        common::multipart_image_job(b"fake-jpeg-bytes", "image/jpeg", common::KNOWN_PROMPT_ID);
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

    // Install a history mock — first empty (pending), then complete —
    // worker polls until the entry materialises.
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

    // Spawn the worker *now* so it picks up the queued job.
    let _handle = common::spawn_worker(&mut app);

    // Poll until done.
    let done = wait_for_status(&router, &job_id, "done", Duration::from_secs(10)).await;
    assert_eq!(done["status"], "done");
    assert!(done["completed_at"].as_i64().unwrap() > 0);

    // Output file should exist on disk with the real PNG bytes.
    let output_rel = format!("outputs/zun_{job_id}_00001_.png");
    let output_abs = app._tempdir.path().join(&output_rel);
    assert!(output_abs.exists(), "output should be at {output_abs:?}");
    let bytes = std::fs::read(&output_abs).unwrap();
    assert_eq!(bytes, png);

    // Worker should have recorded dimensions from the PNG header.
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
    // History reports execution error.
    Mock::given(method("GET"))
        .and(mock_path("/history/bad-prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bad-prompt": {
                "status": { "completed": true, "status_str": "error", "messages": [] },
                "outputs": {}
            }
        })))
        .mount(&comfy)
        .await;

    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());
    let router = app.router.clone();

    let (ct, body) = common::multipart_image_job(b"xxx", "image/jpeg", common::KNOWN_PROMPT_ID);
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
        err.contains("comfyui execution failed"),
        "unexpected error message: {err}"
    );
}

#[tokio::test]
async fn worker_exits_cleanly_when_idle_on_shutdown() {
    // Empty queue: worker should be parked on the idle select. Shutdown
    // should wake it immediately and it should return from run().
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
async fn worker_drains_in_flight_job_then_exits_on_shutdown() {
    // Worker should finish the current job even if shutdown fires while
    // it's in flight (see worker.rs comment: interrupting ComfyUI mid-run
    // leaves orphaned GPU state). To get the worker *into* process_job
    // before firing shutdown, we delay the upload response so the worker
    // is blocked in state.comfy.upload_image when shutdown arrives.
    let comfy = MockServer::start().await;

    Mock::given(method("POST"))
        .and(mock_path("/upload/image"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(400))
                .set_body_json(
                    json!({ "name": "zun_upload.jpg", "subfolder": "", "type": "input" }),
                ),
        )
        .mount(&comfy)
        .await;
    Mock::given(method("POST"))
        .and(mock_path("/prompt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "prompt_id": "drain-prompt" })),
        )
        .mount(&comfy)
        .await;
    let png = common::tiny_png(16, 16);
    Mock::given(method("GET"))
        .and(mock_path("/view"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png.clone()))
        .mount(&comfy)
        .await;

    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    common::seed_workflow(&mut app, "flux2_klein_edit", minimal_workflow());
    let router = app.router.clone();

    let (ct, body) =
        common::multipart_image_job(b"fake-jpeg-bytes", "image/jpeg", common::KNOWN_PROMPT_ID);
    let resp = router
        .clone()
        .oneshot(authed_post_submit(&ct, body))
        .await
        .unwrap();
    let job_id = body_json(resp).await["job_id"]
        .as_str()
        .unwrap()
        .to_string();

    Mock::given(method("GET"))
        .and(mock_path("/history/drain-prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drain-prompt": {
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

    let (handle, shutdown_tx) = common::spawn_worker_with_shutdown(&mut app);

    // Give the worker time to pick up the job and enter process_job (where
    // it now blocks on the delayed upload_image mock). Then fire shutdown.
    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown_tx.send(true).expect("send shutdown");

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("worker did not exit within 5s of shutdown signal")
        .expect("worker task panicked");

    // The in-flight job must have drained to `done`, not left running.
    let final_status = wait_for_status(&router, &job_id, "done", Duration::from_secs(1)).await;
    assert_eq!(final_status["status"], "done");
}

#[tokio::test]
async fn worker_resets_running_jobs_to_queued_on_startup() {
    // Seed a row in `running` state, as if a crash left it stranded.
    let comfy = MockServer::start().await;
    let mut app = common::test_app_with_comfy(&comfy.uri()).await;
    common::seed_job(
        &app.db,
        "stranded",
        "running",
        "test_prompt",
        1_700_000_000,
        None,
    )
    .await;

    // Spawn worker; its first act should be to flip "running" → "queued".
    // We don't need it to actually process (no mocks set up for that).
    let _handle = common::spawn_worker(&mut app);

    // Observe any non-`running` state: worker's first action is to reset
    // `running` → `queued`. Immediately after, it may try to process the
    // newly-queued job against an unmocked comfy URL and race to `failed`.
    // Either outcome proves the reset fired.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let (status,): (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
            .bind("stranded")
            .fetch_one(&app.db)
            .await
            .unwrap();
        if status != "running" {
            // Passed: reset demonstrably ran.
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("stranded running job stayed in `running`; reset did not fire");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
