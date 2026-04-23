//! HTTP client for the ComfyUI API used by project-zun.
//!
//! Speaks exactly four endpoints:
//! - `POST /upload/image` — push an input image, get back the name ComfyUI stored it under.
//! - `POST /prompt` — submit a patched workflow JSON, get back a `prompt_id`.
//! - `GET  /history/{prompt_id}` — poll for completion; empty object until executed.
//! - `GET  /view?filename&subfolder&type` — download an output image.
//!
//! The poll loop and per-job timeout live in the worker (step 4). This module
//! is stateless plumbing plus just enough types to make the history response
//! ergonomic.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde::Deserialize;
use serde_json::Value;

/// Thin wrapper over `reqwest::Client` with a fixed ComfyUI base URL.
#[derive(Clone)]
pub struct ComfyClient {
    base: String,
    /// Client used for job-related traffic; generous 60s timeout since
    /// /upload/image and /view can ship MBs.
    http: Client,
    /// Dedicated client for `/system_stats` health probes with a short
    /// timeout so a stuck ComfyUI doesn't stall the monitor loop.
    health_http: Client,
}

/// Number of attempts (including the first) for idempotent ComfyUI calls.
const MAX_ATTEMPTS: u32 = 3;
/// Base backoff applied as `BASE << (attempt - 1)` between retries.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// How aggressively to retry a given call. `submit_prompt` is not safe to
/// retry after a partially-sent request since ComfyUI may have already
/// accepted the prompt and we'd duplicate work; everything else is a pure
/// read or idempotent write.
#[derive(Copy, Clone)]
enum Idempotency {
    Safe,
    ConnectOnly,
}

fn should_retry(err: &anyhow::Error, mode: Idempotency) -> bool {
    let Some(e) = err.downcast_ref::<reqwest::Error>() else {
        return false;
    };
    match mode {
        Idempotency::ConnectOnly => e.is_connect(),
        Idempotency::Safe => {
            if e.is_connect() || e.is_timeout() {
                return true;
            }
            if let Some(status) = e.status() {
                let code = status.as_u16();
                return code == 408 || code == 429 || status.is_server_error();
            }
            false
        }
    }
}

async fn with_retry<F, Fut, T>(name: &str, mode: Idempotency, mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut attempt: u32 = 1;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= MAX_ATTEMPTS || !should_retry(&e, mode) {
                    return Err(e);
                }
                let delay = BASE_BACKOFF * (1u32 << (attempt - 1));
                tracing::warn!(
                    op = name,
                    attempt,
                    next_delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "transient comfy error; retrying",
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

impl ComfyClient {
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        let http = Client::builder().timeout(Duration::from_secs(60)).build()?;
        let health_http = Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(Self {
            base: base.into(),
            http,
            health_http,
        })
    }

    async fn upload_image_once(&self, bytes: &[u8], filename: &str) -> anyhow::Result<String> {
        let form = Form::new()
            .part(
                "image",
                Part::bytes(bytes.to_vec()).file_name(filename.to_string()),
            )
            .text("type", "input")
            .text("overwrite", "true");
        let resp: Value = self
            .http
            .post(format!("{}/upload/image", self.base))
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        resp["name"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("upload_image response missing `name`: {resp}"))
    }

    /// Upload an input image. Returns the name ComfyUI stored it under
    /// (what a `LoadImage` node should reference).
    pub async fn upload_image(&self, bytes: Vec<u8>, filename: &str) -> anyhow::Result<String> {
        with_retry("upload_image", Idempotency::Safe, || {
            self.upload_image_once(&bytes, filename)
        })
        .await
    }

    async fn submit_prompt_once(&self, workflow: &Value) -> anyhow::Result<String> {
        let resp: Value = self
            .http
            .post(format!("{}/prompt", self.base))
            .json(&serde_json::json!({ "prompt": workflow }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        resp["prompt_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("submit_prompt response missing `prompt_id`: {resp}"))
    }

    /// Submit an already-patched workflow. Returns ComfyUI's prompt id.
    pub async fn submit_prompt(&self, workflow: &Value) -> anyhow::Result<String> {
        // ConnectOnly: a retry after a timeout mid-request could duplicate
        // the submission since ComfyUI may have already accepted it.
        with_retry("submit_prompt", Idempotency::ConnectOnly, || {
            self.submit_prompt_once(workflow)
        })
        .await
    }

    async fn get_history_once(&self, prompt_id: &str) -> anyhow::Result<Option<HistoryEntry>> {
        let resp: Value = self
            .http
            .get(format!("{}/history/{prompt_id}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match resp.get(prompt_id) {
            Some(entry) => Ok(Some(serde_json::from_value(entry.clone())?)),
            None => Ok(None),
        }
    }

    /// Fetch history for a prompt. `Ok(None)` means "not executed yet"
    /// (ComfyUI returns `{}` until the entry materialises). `Ok(Some(...))`
    /// means the entry exists — caller should check `status.status_str`.
    pub async fn get_history(&self, prompt_id: &str) -> anyhow::Result<Option<HistoryEntry>> {
        with_retry("get_history", Idempotency::Safe, || {
            self.get_history_once(prompt_id)
        })
        .await
    }

    /// Lightweight liveness probe. Succeeds if ComfyUI answers `/system_stats`
    /// with a 2xx. Used by the background health monitor. Uses a dedicated
    /// short-timeout client so a stuck ComfyUI doesn't stall the probe loop.
    pub async fn health(&self) -> anyhow::Result<()> {
        self.health_http
            .get(format!("{}/system_stats", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn view_once(
        &self,
        filename: &str,
        subfolder: &str,
        type_: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let resp = self
            .http
            .get(format!("{}/view", self.base))
            .query(&[
                ("filename", filename),
                ("subfolder", subfolder),
                ("type", type_),
            ])
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Download a produced output (image, mask, etc). Bytes are whatever
    /// content-type ComfyUI serves — typically `image/png` for FLUX outputs.
    pub async fn view(
        &self,
        filename: &str,
        subfolder: &str,
        type_: &str,
    ) -> anyhow::Result<Vec<u8>> {
        with_retry("view", Idempotency::Safe, || {
            self.view_once(filename, subfolder, type_)
        })
        .await
    }
}

// ---- /history response types ----

#[derive(Debug, Deserialize)]
pub struct HistoryEntry {
    #[serde(default)]
    pub status: HistoryStatus,
    #[serde(default)]
    pub outputs: HashMap<String, HistoryOutputs>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HistoryStatus {
    #[serde(default)]
    pub completed: bool,
    /// ComfyUI reports `"success"` on success, `"error"` on failure.
    #[serde(default)]
    pub status_str: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HistoryOutputs {
    #[serde(default)]
    pub images: Vec<HistoryImage>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HistoryImage {
    pub filename: String,
    #[serde(default)]
    pub subfolder: String,
    /// Always `"output"` for FLUX workflows, but ComfyUI's API is generic.
    #[serde(rename = "type", default = "default_output_type")]
    pub r#type: String,
}

fn default_output_type() -> String {
    "output".to_string()
}

impl HistoryEntry {
    /// Was execution successful? False also for pending entries (though
    /// `get_history` returns `None` in the pending case, not a half-baked
    /// `HistoryEntry`).
    pub fn succeeded(&self) -> bool {
        self.status.status_str.as_deref() == Some("success")
    }

    /// The primary output image: first `filename` that starts with the
    /// per-job prefix (we set this via `FILENAME_PREFIX_PLACEHOLDER` to
    /// `zun_{job_id}`). Fill workflows also emit `mask_preview_*` side
    /// outputs; filtering by prefix discards those.
    pub fn primary_output(&self, filename_prefix: &str) -> Option<&HistoryImage> {
        self.outputs
            .values()
            .flat_map(|o| o.images.iter())
            .find(|img| img.filename.starts_with(filename_prefix))
    }
}

// ---- tests (wiremock-based; no real ComfyUI) ----

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path as mock_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn upload_image_returns_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(mock_path("/upload/image"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({ "name": "zun_abc.jpg", "subfolder": "", "type": "input" }),
                ),
            )
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let name = client
            .upload_image(b"fake-bytes".to_vec(), "zun_abc.jpg")
            .await
            .unwrap();
        assert_eq!(name, "zun_abc.jpg");
    }

    #[tokio::test]
    async fn upload_image_propagates_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(mock_path("/upload/image"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let err = client
            .upload_image(b"x".to_vec(), "x.jpg")
            .await
            .unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("500"), "expected status in error, got: {s}");
    }

    #[tokio::test]
    async fn submit_prompt_returns_prompt_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(mock_path("/prompt"))
            .and(body_string_contains("PROMPT_PLACEHOLDER"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "prompt_id": "abc-123",
                "number": 1,
                "node_errors": {}
            })))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let wf = json!({ "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } } });
        let pid = client.submit_prompt(&wf).await.unwrap();
        assert_eq!(pid, "abc-123");
    }

    #[tokio::test]
    async fn get_history_returns_none_when_not_yet_executed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/history/pending-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let entry = client.get_history("pending-id").await.unwrap();
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn get_history_parses_success_with_outputs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/history/done-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "done-id": {
                    "status": { "completed": true, "status_str": "success", "messages": [] },
                    "outputs": {
                        "19": {
                            "images": [
                                { "filename": "zun_job1_00001_.png", "subfolder": "", "type": "output" }
                            ]
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let entry = client.get_history("done-id").await.unwrap().unwrap();
        assert!(entry.succeeded());
        let primary = entry.primary_output("zun_job1").unwrap();
        assert_eq!(primary.filename, "zun_job1_00001_.png");
        assert_eq!(primary.r#type, "output");
    }

    #[tokio::test]
    async fn get_history_reports_execution_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/history/bad-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bad-id": {
                    "status": { "completed": true, "status_str": "error", "messages": [] },
                    "outputs": {}
                }
            })))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let entry = client.get_history("bad-id").await.unwrap().unwrap();
        assert!(!entry.succeeded());
        assert!(entry.primary_output("zun_").is_none());
    }

    #[tokio::test]
    async fn primary_output_filters_mask_preview_side_outputs() {
        let entry: HistoryEntry = serde_json::from_value(json!({
            "status": { "status_str": "success" },
            "outputs": {
                "19": { "images": [{ "filename": "zun_myjob_00001_.png" }] },
                "33": { "images": [{ "filename": "mask_preview_raw_00001_.png" }] },
                "34": { "images": [{ "filename": "mask_preview_grown_00001_.png" }] }
            }
        }))
        .unwrap();
        let primary = entry.primary_output("zun_myjob").unwrap();
        assert_eq!(primary.filename, "zun_myjob_00001_.png");
    }

    #[tokio::test]
    async fn view_passes_query_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/view"))
            .and(query_param("filename", "zun_foo.png"))
            .and(query_param("subfolder", ""))
            .and(query_param("type", "output"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG-BYTES"))
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let bytes = client.view("zun_foo.png", "", "output").await.unwrap();
        assert_eq!(bytes, b"PNG-BYTES");
    }

    #[tokio::test]
    async fn upload_image_retries_transient_5xx_then_succeeds() {
        // Two 503 responses followed by a 200 — upload_image should retry
        // and ultimately succeed within MAX_ATTEMPTS.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(mock_path("/upload/image"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(mock_path("/upload/image"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "name": "zun_final.jpg" })),
            )
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let name = client
            .upload_image(b"x".to_vec(), "zun_final.jpg")
            .await
            .expect("retry path should recover");
        assert_eq!(name, "zun_final.jpg");
    }

    #[tokio::test]
    async fn non_transient_4xx_is_not_retried() {
        // A 400 Bad Request should fail on the first attempt with no retry.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(mock_path("/upload/image"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;

        let client = ComfyClient::new(server.uri()).unwrap();
        let err = client
            .upload_image(b"x".to_vec(), "x.jpg")
            .await
            .expect_err("400 is not transient");
        let s = format!("{err}");
        assert!(s.contains("400"), "expected status in error, got: {s}");
        // Drop the server — on drop wiremock verifies `.expect(1)`.
    }
}
