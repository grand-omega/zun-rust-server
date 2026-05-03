//! Shared test scaffolding. Builds a `TestApp` that owns a tempdir, a
//! migrated SQLite, and a fully-wired router. Helpers for seeding rows
//! and constructing multipart submit bodies live here too.

use std::sync::Arc;

use axum::Router;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::{mpsc, watch};
use zun_rust_server::{
    AppState, Config, auth::AuthLimiter, comfy::ComfyClient, comfy_monitor, db, hash, router,
    state::UserId, worker, workflow,
};

pub const TEST_TOKEN: &str = "test-token-0123456789abcdef";

/// User id seeded by `db::init`.
pub const TEST_USER: UserId = UserId(1);

pub struct TestApp {
    pub router: Router,
    #[allow(dead_code)]
    pub db: SqlitePool,
    #[allow(dead_code)]
    pub state: AppState,
    #[allow(dead_code)]
    pub worker_rx: Option<mpsc::Receiver<()>>,
    pub _tempdir: TempDir,
}

pub async fn test_app_with_comfy(comfy_url: &str) -> TestApp {
    test_app_with_comfy_and_ws(comfy_url, "ws://127.0.0.1:1").await
}

#[allow(dead_code)]
pub async fn test_app_with_comfy_and_ws(comfy_url: &str, ws_url: &str) -> TestApp {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let pool = db::init(tempdir.path()).await.expect("init db");

    let config = Config {
        data_dir: tempdir.path().to_path_buf(),
        workflows_dir: None,
        diffusers_model_path: None,
        bind: "127.0.0.1:0".into(),
        token: TEST_TOKEN.to_string(),
        comfy_url: comfy_url.to_string(),
        log_format: zun_rust_server::config::LogFormat::Auto,
    };
    let comfy = ComfyClient::with_ws_base(comfy_url, ws_url).expect("comfy client");
    let (worker_tx, worker_rx) = mpsc::channel::<()>(1);

    let state = AppState {
        db: pool.clone(),
        config,
        workflows: Arc::new(workflow::WorkflowRegistry::empty()),
        comfy,
        comfy_health: comfy_monitor::new_handle(),
        worker_tx,
        auth_limiter: AuthLimiter::new(),
        disk_usage_cache: Arc::new(parking_lot::Mutex::new(None)),
    };
    TestApp {
        router: router(state.clone()),
        db: pool,
        state,
        worker_rx: Some(worker_rx),
        _tempdir: tempdir,
    }
}

#[allow(dead_code)]
pub async fn test_app() -> TestApp {
    test_app_with_comfy("http://127.0.0.1:1").await
}

#[allow(dead_code)]
pub fn spawn_worker(app: &mut TestApp) -> tokio::task::JoinHandle<()> {
    let rx = app
        .worker_rx
        .take()
        .expect("worker already spawned for this TestApp");
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    worker::spawn(app.state.clone(), rx, shutdown_rx)
}

#[allow(dead_code)]
pub fn spawn_worker_with_shutdown(
    app: &mut TestApp,
) -> (tokio::task::JoinHandle<()>, watch::Sender<bool>) {
    let rx = app
        .worker_rx
        .take()
        .expect("worker already spawned for this TestApp");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = worker::spawn(app.state.clone(), rx, shutdown_rx);
    (handle, shutdown_tx)
}

#[allow(dead_code)]
pub fn seed_workflow(app: &mut TestApp, stem: &str, template: serde_json::Value) {
    let mut templates = app.state.workflows.templates.clone();
    templates.insert(stem.to_string(), template);
    let support = workflow::analyze_templates(&templates);
    let arc = Arc::new(workflow::WorkflowRegistry { templates, support });
    app.state.workflows = arc.clone();
    app.router = router(app.state.clone());
}

#[allow(dead_code)]
pub fn supported_edit_workflow() -> serde_json::Value {
    serde_json::json!({
        "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" }, "class_type": "LoadImage" },
        "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" }, "class_type": "CLIPTextEncode" },
        "16": { "inputs": { "noise_seed": "SEED_PLACEHOLDER" }, "class_type": "RandomNoise" },
        "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" }, "class_type": "SaveImage" }
    })
}

/// Insert a custom_prompts row for the test user. Returns the new id.
#[allow(dead_code)]
pub async fn seed_prompt(db: &SqlitePool, label: &str, text: &str, workflow: &str) -> i64 {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        "INSERT INTO custom_prompts (user_id, label, description, text, workflow, created_at, updated_at) \
         VALUES (?, ?, NULL, ?, ?, ?, ?)",
    )
    .bind(TEST_USER.0)
    .bind(label)
    .bind(text)
    .bind(workflow)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .expect("seed_prompt");
    res.last_insert_rowid()
}

/// Insert an inputs row, optionally writing a fake file. Returns input_id.
#[allow(dead_code)]
pub async fn seed_input(
    db: &SqlitePool,
    tempdir: &std::path::Path,
    sha256: &str,
    bytes: Option<&[u8]>,
) -> i64 {
    let now = chrono::Utc::now().timestamp();
    let path = if let Some(b) = bytes {
        let rel = format!("users/1/cache/inputs/{sha256}.jpg");
        let abs = tempdir.join(&rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b).unwrap();
        Some(rel)
    } else {
        None
    };
    let res = sqlx::query(
        "INSERT INTO inputs (user_id, sha256, path, content_type, size_bytes, created_at, last_used_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(TEST_USER.0)
    .bind(sha256)
    .bind(&path)
    .bind("image/jpeg")
    .bind(bytes.map(|b| b.len() as i64))
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .expect("seed_input");
    res.last_insert_rowid()
}

/// Insert a jobs row directly. Provide an input_id (use seed_input).
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn seed_job(
    db: &SqlitePool,
    id: &str,
    status: &str,
    prompt_id: Option<i64>,
    prompt_text: Option<&str>,
    workflow: &str,
    input_id: i64,
    created_at: i64,
    completed_at: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO jobs (id, user_id, input_id, prompt_id, prompt_text, workflow, seed, status, created_at, completed_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(TEST_USER.0)
    .bind(input_id)
    .bind(prompt_id)
    .bind(prompt_text)
    .bind(workflow)
    .bind(0_i64)
    .bind(status)
    .bind(created_at)
    .bind(completed_at)
    .execute(db)
    .await
    .expect("seed_job");
}

#[allow(dead_code)]
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[allow(dead_code)]
pub async fn start_ws_mock(frames: Vec<String>) -> String {
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let frames = frames.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = accept_async(stream).await else {
                    return;
                };
                for f in frames {
                    if ws.send(Message::text(f)).await.is_err() {
                        return;
                    }
                }
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    format!("ws://{addr}")
}

#[allow(dead_code)]
pub fn ws_success_frame(prompt_id: &str) -> String {
    format!(r#"{{"type":"executing","data":{{"node":null,"prompt_id":"{prompt_id}"}}}}"#)
}

#[allow(dead_code)]
pub fn tiny_png(width: u32, height: u32) -> Vec<u8> {
    use image::{DynamicImage, ImageFormat, RgbImage};
    let img = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([((x * 17) % 255) as u8, ((y * 31) % 255) as u8, 128])
    });
    let mut buf: Vec<u8> = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
        .expect("encode tiny png");
    buf
}

/// Multipart body for POST /api/v1/jobs. Computes sha256 of `image_bytes`
/// and includes it as input_sha256.
#[allow(dead_code)]
pub fn multipart_submit(
    image_bytes: &[u8],
    content_type: &str,
    prompt_id: Option<i64>,
    prompt_text: Option<&str>,
    workflow: Option<&str>,
) -> (String, Vec<u8>) {
    let boundary = "----ZunTestBoundary9XyZ";
    let sha = hash::sha256_hex(image_bytes);
    let mut body: Vec<u8> = Vec::new();
    let push_text = |body: &mut Vec<u8>, name: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    };
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"image\"; filename=\"t.bin\"\r\n",
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(image_bytes);
    body.extend_from_slice(b"\r\n");
    push_text(&mut body, "input_sha256", &sha);
    if let Some(pid) = prompt_id {
        push_text(&mut body, "prompt_id", &pid.to_string());
    }
    if let Some(t) = prompt_text {
        push_text(&mut body, "prompt_text", t);
    }
    if let Some(wf) = workflow {
        push_text(&mut body, "workflow", wf);
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Multipart body without the image field — for "missing image" tests.
#[allow(dead_code)]
pub fn multipart_no_image(prompt_id: i64) -> (String, Vec<u8>) {
    let boundary = "----ZunTestBoundary9XyZ";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt_id\"\r\n\r\n");
    body.extend_from_slice(prompt_id.to_string().as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"input_sha256\"\r\n\r\n");
    body.extend_from_slice(b"deadbeef");
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}
