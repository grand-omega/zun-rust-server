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
        .uri("/api/jobs")
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", content_type)
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn list_prompts_returns_public_fields_only() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed_get("/api/prompts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let items = body.as_array().unwrap();
    // 2 from prompts.yaml + 1 synthetic __custom__ entry
    assert_eq!(items.len(), 3);

    for item in items {
        assert!(item.get("id").is_some());
        assert!(item.get("label").is_some());
        // public projection must not leak internals:
        assert!(item.get("text").is_none());
        assert!(item.get("workflow").is_none());
    }
}

#[tokio::test]
async fn list_prompts_requires_auth() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/prompts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn submit_valid_job_returns_202_with_location_and_job_id() {
    let app = common::test_app().await;
    let router = app.router.clone();
    let (ct, body) =
        common::multipart_image_job(b"fake-jpeg-bytes", "image/jpeg", common::KNOWN_PROMPT_ID);

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
    assert_eq!(job_id.len(), 36);
    assert_eq!(location, format!("/api/jobs/{job_id}"));

    // Input file was written to the tempdir.
    let expected = app._tempdir.path().join(format!("inputs/{job_id}.jpg"));
    assert!(
        expected.exists(),
        "input file should be written to {expected:?}"
    );

    // And the job is queryable with status=queued.
    let uri = format!("/api/jobs/{job_id}");
    let resp = router.oneshot(authed_get(&uri)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["id"], job_id);
    assert_eq!(status["status"], "queued");
    assert_eq!(status["prompt_id"], common::KNOWN_PROMPT_ID);
    assert_eq!(status["prompt_label"], "Test Prompt");
    assert!(status["progress"].is_null());
    assert!(status["error"].is_null());
    assert!(status["created_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn submit_unknown_prompt_id_is_400() {
    let app = common::test_app().await;
    let (ct, body) = common::multipart_image_job(b"x", "image/jpeg", "no_such_prompt");
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "invalid_prompt_id");
}

#[tokio::test]
async fn submit_missing_image_is_400() {
    let app = common::test_app().await;
    let (ct, body) = common::multipart_no_image(common::KNOWN_PROMPT_ID);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn submit_unsupported_content_type_is_400() {
    let app = common::test_app().await;
    let (ct, body) = common::multipart_image_job(b"x", "image/gif", common::KNOWN_PROMPT_ID);
    let resp = app.router.oneshot(submit_request(&ct, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_requires_auth() {
    let app = common::test_app().await;
    let (ct, body) = common::multipart_image_job(b"x", "image/jpeg", common::KNOWN_PROMPT_ID);
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_unknown_job_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed_get("/api/jobs/00000000-0000-0000-0000-000000000000"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
