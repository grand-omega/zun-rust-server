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

#[tokio::test]
async fn list_empty_initially() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/debug/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn create_then_list_returns_the_row() {
    let app = common::test_app().await;
    let router = app.router.clone();

    // Insert a debug job.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debug/job")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 36, "uuid v4 string length");

    // List and find it.
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/debug/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id);
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["prompt_id"], "debug");
    assert!(items[0]["created_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn multiple_inserts_ordered_newest_first() {
    let app = common::test_app().await;
    let router = app.router.clone();

    let mut ids = Vec::new();
    for _ in 0..3 {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/debug/job")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        ids.push(body["id"].as_str().unwrap().to_string());
        // Space timestamps by a second to get deterministic ordering.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/debug/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(resp).await;
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 3);
    // Newest first: last inserted id should be the first listed.
    assert_eq!(items[0]["id"], ids[2]);
    assert_eq!(items[2]["id"], ids[0]);
}
