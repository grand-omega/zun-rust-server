use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method, matchers::path as mock_path};

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
async fn cancel_queued_job_does_not_interrupt_comfy() {
    // Regression test: the worker runs exactly one job at a time, so
    // cancelling a merely-queued job must NOT call ComfyUI's /interrupt —
    // that would abort a different, unrelated job that's actually running.
    let comfy = MockServer::start().await;
    Mock::given(method("POST"))
        .and(mock_path("/interrupt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&comfy)
        .await;

    let app = common::test_app_with_comfy(&comfy.uri()).await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-cancel-queued",
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
        .oneshot(authed("POST", "/api/v1/jobs/j-cancel-queued/cancel"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status,): (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind("j-cancel-queued")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    // Drop of `comfy` verifies the `.expect(0)` mount above.
}

#[tokio::test]
async fn cancel_running_job_interrupts_comfy() {
    let comfy = MockServer::start().await;
    Mock::given(method("POST"))
        .and(mock_path("/interrupt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&comfy)
        .await;

    let app = common::test_app_with_comfy(&comfy.uri()).await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "j-cancel-running",
        "running",
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
        .oneshot(authed("POST", "/api/v1/jobs/j-cancel-running/cancel"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status,): (String,) = sqlx::query_as("SELECT status FROM jobs WHERE id = ?")
        .bind("j-cancel-running")
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

#[tokio::test]
async fn get_job_reports_queue_position_for_queued_jobs() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    for (id, ts) in [("q1", 100), ("q2", 200), ("q3", 300)] {
        common::seed_job(
            &app.db,
            id,
            "queued",
            None,
            Some("p"),
            "flux2_klein_edit",
            input_id,
            ts,
            None,
        )
        .await;
    }

    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/q1"))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["queue_position"], 0);

    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/q3"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["queue_position"], 2);
    assert_eq!(body["progress"], 0.0);
}

#[tokio::test]
async fn get_job_queue_position_is_null_for_done_jobs() {
    let app = common::test_app().await;
    seed_inputs_and_jobs(&app, &[("d1", 100)]).await;

    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/d1"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert!(body["queue_position"].is_null());
}

#[tokio::test]
async fn get_job_wait_returns_early_on_status_change() {
    let app = common::test_app().await;
    let input_id =
        common::seed_input(&app.db, app._tempdir.path(), &"a".repeat(64), Some(b"a")).await;
    common::seed_job(
        &app.db,
        "lp1",
        "queued",
        None,
        Some("p"),
        "flux2_klein_edit",
        input_id,
        100,
        None,
    )
    .await;

    // Flip the job to done shortly after the long-poll starts.
    let db = app.db.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        sqlx::query("UPDATE jobs SET status = 'done', progress = 1.0 WHERE id = 'lp1'")
            .execute(&db)
            .await
            .unwrap();
    });

    let started = std::time::Instant::now();
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/lp1?wait=10"))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let body = body_json(resp).await;
    assert_eq!(body["status"], "done");
    assert_eq!(body["progress"], 1.0);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "long-poll should return on change, took {elapsed:?}"
    );
}

#[tokio::test]
async fn get_job_wait_returns_immediately_for_terminal_jobs() {
    let app = common::test_app().await;
    seed_inputs_and_jobs(&app, &[("t1", 100)]).await;

    let started = std::time::Instant::now();
    let resp = app
        .router
        .clone()
        .oneshot(authed("GET", "/api/v1/jobs/t1?wait=10"))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "terminal job must not hold the long-poll window, took {elapsed:?}"
    );
}
