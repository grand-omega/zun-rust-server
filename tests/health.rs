use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

mod common;

fn authed_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn health_returns_ok_and_version() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn health_reports_comfy_reachability_shape() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["comfy"]["ok"], false);
    assert!(body["comfy"]["last_ok_at"].is_null());
    assert_eq!(body["comfy"]["consecutive_failures"], 0);
}

#[tokio::test]
async fn response_carries_x_request_id() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let id = resp
        .headers()
        .get("x-request-id")
        .expect("x-request-id set on response")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(id.len(), 36, "uuid v4 string length");
}

#[tokio::test]
async fn client_supplied_request_id_is_propagated() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .header("x-request-id", "client-supplied-id-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let id = resp
        .headers()
        .get("x-request-id")
        .expect("x-request-id set on response")
        .to_str()
        .unwrap();
    assert_eq!(id, "client-supplied-id-123");
}

#[tokio::test]
async fn health_includes_disk_usage_field() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // Field is present even with no data dir yet.
    assert!(body["disk"]["data_bytes"].as_u64().is_some());
}

#[tokio::test]
async fn capabilities_reports_enabled_workflows() {
    let mut app = common::test_app().await;
    common::seed_workflow(
        &mut app,
        "flux2_klein_edit",
        common::supported_edit_workflow(),
    );

    let resp = app
        .router
        .oneshot(authed_get("/api/v1/capabilities"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["features"]["image_edit"], true);

    let workflows = body["workflows"].as_array().unwrap();
    let supported = workflows
        .iter()
        .find(|w| w["name"] == "flux2_klein_edit")
        .unwrap();
    assert_eq!(supported["supported"], true);
    assert_eq!(supported["display_name"], "FLUX 2 klein");
    assert_eq!(supported["default"], true);
    assert!(
        workflows.iter().all(|w| w["name"] != "flux_fill_auto_mask"),
        "capabilities should only expose enabled workflows"
    );
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
