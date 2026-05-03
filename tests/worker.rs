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
