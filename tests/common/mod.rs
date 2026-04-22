use std::sync::Arc;

use axum::Router;
use tempfile::TempDir;
use zun_rust_server::{AppState, Config, db, prompts, router};

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
    pub _tempdir: TempDir,
}

pub async fn test_app() -> TestApp {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let pool = db::init(tempdir.path()).await.expect("init db");

    let prompts_path = tempdir.path().join("prompts.yaml");
    std::fs::write(&prompts_path, TEST_PROMPTS_YAML).expect("write test prompts");
    let prompts_map = prompts::load(&prompts_path).expect("parse test prompts");

    let config = Config {
        data_dir: tempdir.path().to_path_buf(),
        bind_addr: "127.0.0.1:0".to_string(),
        token: TEST_TOKEN.to_string(),
    };
    let state = AppState {
        db: pool,
        config,
        prompts: Arc::new(prompts_map),
    };
    TestApp {
        router: router(state),
        _tempdir: tempdir,
    }
}

/// Build `Authorization: Bearer <token>` header value.
#[allow(dead_code)]
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Build a multipart/form-data body for submitting a job.
/// Returns (content_type_header_value, body_bytes).
#[allow(dead_code)]
pub fn multipart_image_job(
    image_bytes: &[u8],
    content_type: &str,
    prompt_id: &str,
) -> (String, Vec<u8>) {
    let boundary = "----ZunTestBoundary9XyZ";
    let mut body: Vec<u8> = Vec::new();

    // image field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"image\"; filename=\"t.bin\"\r\n",
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(image_bytes);
    body.extend_from_slice(b"\r\n");

    // prompt_id field
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
