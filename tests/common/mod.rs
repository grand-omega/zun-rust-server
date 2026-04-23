use std::sync::Arc;

use axum::Router;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::{mpsc, watch};
use zun_rust_server::{
    AppState, Config, auth::AuthLimiter, comfy::ComfyClient, comfy_monitor, db, prompts, router,
    worker,
};

/// Bearer token used by all tests.
pub const TEST_TOKEN: &str = "test-token-0123456789abcdef";

/// Prompt ids that `test_app` seeds into `prompts.yaml`.
#[allow(dead_code)]
pub const KNOWN_PROMPT_ID: &str = "test_prompt";
#[allow(dead_code)]
pub const MASK_PROMPT_ID: &str = "test_mask";

const TEST_PROMPTS_YAML: &str = "\
prompts:
  - id: test_prompt
    label: Test Prompt
    description: A stand-in prompt for tests
    text: test prompt text
    workflow: flux2_klein_edit

  - id: test_mask
    label: Test Mask
    text: test mask prompt
    workflow: flux_fill_auto_mask
";

/// A fully-wired test app backed by a fresh temp-dir SQLite and a
/// seeded prompts.yaml. Keep `_tempdir` alive for the test lifetime.
pub struct TestApp {
    pub router: Router,
    // Other test binaries don't need direct DB access; gallery/worker tests do.
    #[allow(dead_code)]
    pub db: SqlitePool,
    #[allow(dead_code)]
    pub state: AppState,
    /// The receiver half of the wake channel — take this and pass to
    /// `worker::spawn` in tests that need the worker running. Tests that
    /// don't need a worker can ignore it (submitted jobs just stay queued).
    #[allow(dead_code)]
    pub worker_rx: Option<mpsc::Receiver<()>>,
    pub _tempdir: TempDir,
}

/// Build a TestApp with `comfy_url` pointing at a wiremock server (or any
/// URL — if no worker is spawned, it's never called).
pub async fn test_app_with_comfy(comfy_url: &str) -> TestApp {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let pool = db::init(tempdir.path()).await.expect("init db");

    let prompts_path = tempdir.path().join("prompts.yaml");
    std::fs::write(&prompts_path, TEST_PROMPTS_YAML).expect("write test prompts");
    let prompts_map = prompts::load(&prompts_path).expect("parse test prompts");

    let config = Config {
        data_dir: tempdir.path().to_path_buf(),
        bind_addr: "127.0.0.1:0".to_string(),
        token: TEST_TOKEN.to_string(),
        comfy_url: comfy_url.to_string(),
    };
    let comfy = ComfyClient::new(comfy_url).expect("comfy client");
    let (worker_tx, worker_rx) = mpsc::channel::<()>(1);

    let state = AppState {
        db: pool.clone(),
        config,
        prompts: Arc::new(prompts_map),
        workflows: Arc::new(std::collections::HashMap::new()),
        comfy,
        comfy_health: comfy_monitor::new_handle(),
        worker_tx,
        auth_limiter: AuthLimiter::new(),
    };
    TestApp {
        router: router(state.clone()),
        db: pool,
        state,
        worker_rx: Some(worker_rx),
        _tempdir: tempdir,
    }
}

/// Convenience: TestApp with an unreachable comfy URL. Use when the test
/// doesn't spawn a worker, so the URL never gets dialled.
#[allow(dead_code)]
pub async fn test_app() -> TestApp {
    test_app_with_comfy("http://127.0.0.1:1").await
}

/// Spawn the worker consuming the TestApp's wake channel. Call once per
/// TestApp; subsequent calls panic because the rx is already taken.
/// The shutdown receiver is held only by the worker; dropping the sender
/// side in this helper means the worker runs for the test lifetime.
#[allow(dead_code)]
pub fn spawn_worker(app: &mut TestApp) -> tokio::task::JoinHandle<()> {
    let rx = app
        .worker_rx
        .take()
        .expect("worker already spawned for this TestApp");
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    // _shutdown_tx dropped here — receiver still sees `false` for the
    // lifetime of the test. The #[tokio::test] runtime drops the worker
    // task when the test returns.
    worker::spawn(app.state.clone(), rx, shutdown_rx)
}

/// Seed a workflow template into AppState (tests bypass the filesystem
/// loader). Takes &mut AppState so we can rebuild the Arc<HashMap>.
#[allow(dead_code)]
pub fn seed_workflow(app: &mut TestApp, stem: &str, template: serde_json::Value) {
    let mut map = (*app.state.workflows).clone();
    map.insert(stem.to_string(), template);
    let arc = Arc::new(map);
    app.state.workflows = arc.clone();
    // Rebuild the router so its State<AppState> sees the updated workflows.
    app.router = router(app.state.clone());
}

/// Insert a jobs row directly, bypassing the submit handler. Useful for
/// seeding list/delete tests without going through the multipart path.
#[allow(dead_code)]
pub async fn seed_job(
    db: &SqlitePool,
    id: &str,
    status: &str,
    prompt_id: &str,
    created_at: i64,
    completed_at: Option<i64>,
) {
    sqlx::query(
        "INSERT INTO jobs (id, status, prompt_id, input_path, created_at, completed_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(prompt_id)
    .bind(format!("inputs/{id}.jpg"))
    .bind(created_at)
    .bind(completed_at)
    .execute(db)
    .await
    .expect("seed_job insert");
}

/// Build `Authorization: Bearer <token>` header value.
#[allow(dead_code)]
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Build a tiny real PNG (decodable by the `image` crate) for tests that
/// need valid image bytes — e.g., worker output validation and thumbnail
/// generation.
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

/// Build a multipart/form-data body for submitting a job.
#[allow(dead_code)]
pub fn multipart_image_job(
    image_bytes: &[u8],
    content_type: &str,
    prompt_id: &str,
) -> (String, Vec<u8>) {
    let boundary = "----ZunTestBoundary9XyZ";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"image\"; filename=\"t.bin\"\r\n",
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(image_bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt_id\"\r\n\r\n");
    body.extend_from_slice(prompt_id.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Same as above but omits the image field (for "missing image" tests).
#[allow(dead_code)]
pub fn multipart_no_image(prompt_id: &str) -> (String, Vec<u8>) {
    let boundary = "----ZunTestBoundary9XyZ";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt_id\"\r\n\r\n");
    body.extend_from_slice(prompt_id.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}
