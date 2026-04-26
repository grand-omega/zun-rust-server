use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

mod common;

fn req(uri: &str, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri(uri);
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_is_public_no_auth_needed() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(req("/api/v1/health", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn authed_route_rejects_missing_header() {
    let app = common::test_app().await;
    let resp = app.router.oneshot(req("/api/v1/jobs", None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authed_route_rejects_wrong_token() {
    let app = common::test_app().await;
    let bad = common::bearer("wrong-token-0123456789abcdef");
    let resp = app
        .router
        .oneshot(req("/api/v1/jobs", Some(&bad)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authed_route_rejects_missing_bearer_prefix() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(req("/api/v1/jobs", Some(common::TEST_TOKEN)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authed_route_accepts_correct_token() {
    let app = common::test_app().await;
    let good = common::bearer(common::TEST_TOKEN);
    let resp = app
        .router
        .oneshot(req("/api/v1/jobs", Some(&good)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
