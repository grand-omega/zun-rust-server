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

async fn seed_inputs_and_jobs(app: &common::TestApp, ids: &[(&str, i64)]) {
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    for (id, ts) in ids {
        common::seed_job(
            &app.db,
            id,
            "done",
            None,
            Some("p"),
            "flux2_klein_edit",
            input_id,
            *ts,
            Some(*ts + 5),
        )
        .await;
    }
}

#[tokio::test]
async fn list_empty_returns_empty_items() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/jobs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn list_filters_by_status() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-done",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;
    common::seed_job(
        &app.db,
        "j-queued",
        "queued",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_100,
        None,
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs?status=done"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "j-done");

    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/jobs?status=queued"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "j-queued");
}

#[tokio::test]
async fn list_pagination_with_cursor_round_trips() {
    let app = common::test_app().await;
    let pairs: Vec<(String, i64)> = (0..5)
        .map(|i| (format!("j{i}"), 1_700_000_000 + i))
        .collect();
    let pairs_ref: Vec<(&str, i64)> = pairs.iter().map(|(s, t)| (s.as_str(), *t)).collect();
    seed_inputs_and_jobs(&app, &pairs_ref).await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs?limit=2"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "j4");
    assert_eq!(items[1]["id"], "j3");
    let cursor = body["next_cursor"].as_str().unwrap().to_string();

    let uri = format!("/api/v1/jobs?limit=2&cursor={cursor}");
    let resp = app.router.oneshot(authed("GET", &uri)).await.unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "j2");
    assert_eq!(items[1]["id"], "j1");
}

#[tokio::test]
async fn delete_is_soft_and_restorable() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-soft",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;

    // Delete (soft).
    let resp = app
        .router
        .clone()
        .oneshot(authed("DELETE", "/api/v1/jobs/j-soft"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Now invisible from GET.
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/j-soft"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Restore.
    let resp = app
        .router
        .clone()
        .oneshot(authed("POST", "/api/v1/jobs/j-soft/restore"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Visible again.
    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/jobs/j-soft"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_excludes_soft_deleted() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-keep",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;
    common::seed_job(
        &app.db,
        "j-gone",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_100,
        Some(1_700_000_110),
    )
    .await;
    sqlx::query("UPDATE jobs SET deleted_at = ? WHERE id = ?")
        .bind(1_700_000_999_i64)
        .bind("j-gone")
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .router
        .oneshot(authed("GET", "/api/v1/jobs"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "j-keep");
}

#[tokio::test]
async fn cancel_queued_job_marks_cancelled() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-cancel",
        "queued",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        None,
    )
    .await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("POST", "/api/v1/jobs/j-cancel/cancel"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status,): (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind("j-cancel")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
}

#[tokio::test]
async fn cancel_already_done_job_is_404() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-done",
        "done",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        1_700_000_000,
        Some(1_700_000_010),
    )
    .await;
    let resp = app
        .router
        .oneshot(authed("POST", "/api/v1/jobs/j-done/cancel"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_requires_auth() {
    let app = common::test_app().await;
    let resp = app
        .router
        .oneshot(
            Request::builder()
                .uri("/api/v1/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
