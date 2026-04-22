use axum::Router;
use tempfile::TempDir;
use zun_rust_server::{AppState, Config, db, router};

/// Bearer token used by all tests.
pub const TEST_TOKEN: &str = "test-token-0123456789abcdef";

/// A fully-wired test app backed by a fresh temp-dir SQLite.
/// Keep `_tempdir` alive for the lifetime of the test — drop cleans up.
pub struct TestApp {
    pub router: Router,
    pub _tempdir: TempDir,
}

pub async fn test_app() -> TestApp {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let pool = db::init(tempdir.path()).await.expect("init db");
    let config = Config {
        data_dir: tempdir.path().to_path_buf(),
        bind_addr: "127.0.0.1:0".to_string(),
        token: TEST_TOKEN.to_string(),
    };
    let state = AppState { db: pool, config };
    TestApp {
        router: router(state),
        _tempdir: tempdir,
    }
}

/// Build `Authorization: Bearer <token>` header value.
/// `allow(dead_code)` because each integration-test binary compiles `common`
/// independently and may not use every helper.
#[allow(dead_code)]
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}
