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
    common::seed_workflow(
        &mut app,
        "flux3_klein_edit",
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
    let names: Vec<_> = workflows.iter().map(|w| w["name"].clone()).collect();
    assert_eq!(names, ["flux2_klein_edit", "flux3_klein_edit"]);

    let flux3 = &workflows[1];
    assert_eq!(flux3["supported"], true);
    assert_eq!(flux3["display_name"], "FLUX 3 klein");
    assert_eq!(flux3["default"], true);

    let flux2 = &workflows[0];
    assert_eq!(flux2["supported"], true);
    assert_eq!(flux2["display_name"], "FLUX 2 klein");
    assert_eq!(flux2["default"], false);
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
