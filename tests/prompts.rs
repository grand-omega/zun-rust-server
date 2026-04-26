use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::json;
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

fn authed_json(method: &'static str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", common::bearer(common::TEST_TOKEN))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn app_with_workflow() -> common::TestApp {
    let mut app = common::test_app().await;
    common::seed_workflow(&mut app, "flux2_klein_edit", json!({}));
    app
}

#[tokio::test]
async fn list_requires_auth() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/prompts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_requires_auth() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/prompts")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_empty_returns_no_items() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/prompts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn create_returns_201_with_row() {
    let app = app_with_workflow().await;
    let resp = app
        .router
        .oneshot(authed_json(
            "POST",
            "/api/v1/prompts",
            json!({
                "label": "My prompt",
                "description": "A note",
                "text": "do the thing",
                "workflow": "flux2_klein_edit",
                "timeout_seconds": 90,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let row = body_json(resp).await;
    assert!(row["id"].as_i64().unwrap() > 0);
    assert_eq!(row["label"], "My prompt");
    assert_eq!(row["description"], "A note");
    assert_eq!(row["text"], "do the thing");
    assert_eq!(row["workflow"], "flux2_klein_edit");
    assert_eq!(row["timeout_seconds"], 90);
    assert_eq!(row["created_at"], row["updated_at"]);
}

#[tokio::test]
async fn create_with_empty_label_is_400() {
    let app = app_with_workflow().await;
    let resp = app
        .router
        .oneshot(authed_json(
            "POST",
            "/api/v1/prompts",
            json!({
                "label": "   ",
                "text": "hi",
                "workflow": "flux2_klein_edit",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_with_empty_text_is_400() {
    let app = app_with_workflow().await;
    let resp = app
        .router
        .oneshot(authed_json(
            "POST",
            "/api/v1/prompts",
            json!({
                "label": "x",
                "text": "",
                "workflow": "flux2_klein_edit",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_with_unknown_workflow_is_400() {
    let app = app_with_workflow().await;
    let resp = app
        .router
        .oneshot(authed_json(
            "POST",
            "/api/v1/prompts",
            json!({
                "label": "x",
                "text": "y",
                "workflow": "nope_not_a_workflow",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_returns_created_rows_in_created_at_order() {
    let app = app_with_workflow().await;
    let id_a = common::seed_prompt(&app.db, "A", "ta", "flux2_klein_edit").await;
    // Force a strictly later created_at so order is deterministic.
    sqlx::query("UPDATE custom_prompts SET created_at = created_at + 1, updated_at = updated_at + 1 WHERE id = ?")
        .bind(id_a)
        .execute(&app.db)
        .await
        .unwrap();
    let id_b = common::seed_prompt(&app.db, "B", "tb", "flux2_klein_edit").await;

    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/prompts"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], id_b);
    assert_eq!(items[1]["id"], id_a);
}

#[tokio::test]
async fn get_one_returns_row() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "L", "T", "flux2_klein_edit").await;
    let resp = app
        .router
        .oneshot(authed("GET", &format!("/api/v1/prompts/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = body_json(resp).await;
    assert_eq!(row["id"], id);
    assert_eq!(row["label"], "L");
}

#[tokio::test]
async fn get_one_unknown_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/prompts/9999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_partial_label_only() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "Old", "T", "flux2_klein_edit").await;
    let resp = app
        .router
        .oneshot(authed_json(
            "PATCH",
            &format!("/api/v1/prompts/{id}"),
            json!({ "label": "New" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = body_json(resp).await;
    assert_eq!(row["label"], "New");
    assert_eq!(row["text"], "T");
    assert_eq!(row["workflow"], "flux2_klein_edit");
}

#[tokio::test]
async fn update_can_set_description() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "L", "T", "flux2_klein_edit").await;
    let resp = app
        .router
        .oneshot(authed_json(
            "PATCH",
            &format!("/api/v1/prompts/{id}"),
            json!({ "description": "hello" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["description"], "hello");
}

#[tokio::test]
async fn update_with_empty_label_is_400() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "L", "T", "flux2_klein_edit").await;
    let resp = app
        .router
        .oneshot(authed_json(
            "PATCH",
            &format!("/api/v1/prompts/{id}"),
            json!({ "label": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_with_unknown_workflow_is_400() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "L", "T", "flux2_klein_edit").await;
    let resp = app
        .router
        .oneshot(authed_json(
            "PATCH",
            &format!("/api/v1/prompts/{id}"),
            json!({ "workflow": "ghost" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_unknown_id_is_404() {
    let app = app_with_workflow().await;
    let resp = app
        .router
        .oneshot(authed_json(
            "PATCH",
            "/api/v1/prompts/9999",
            json!({ "label": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_is_soft_and_subsequent_get_is_404() {
    let app = app_with_workflow().await;
    let id = common::seed_prompt(&app.db, "L", "T", "flux2_klein_edit").await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("DELETE", &format!("/api/v1/prompts/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Hidden from GET.
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", &format!("/api/v1/prompts/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Hidden from list.
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/prompts"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);

    // Row still in the table with deleted_at populated.
    let (deleted_at,): (Option<i64>,) =
        sqlx::query_as("SELECT deleted_at FROM custom_prompts WHERE id = ?")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(deleted_at.is_some());

    // Second delete is 404 (no live row to soft-delete).
    let resp = app
        .router
        .oneshot(authed("DELETE", &format!("/api/v1/prompts/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_unknown_id_is_404() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("DELETE", "/api/v1/prompts/9999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
