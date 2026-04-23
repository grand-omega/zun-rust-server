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
    http: Client,
}

impl ComfyClient {
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        let http = Client::builder()
            // Generous per-request timeout. The worker wraps the poll loop in
            // its own overall timeout; this just protects against a single
            // stuck TCP dial.
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: base.into(),
            http,
        })
    }

    /// Upload an input image. Returns the name ComfyUI stored it under
    /// (what a `LoadImage` node should reference).
    pub async fn upload_image(&self, bytes: Vec<u8>, filename: &str) -> anyhow::Result<String> {
        let form = Form::new()
            .part("image", Part::bytes(bytes).file_name(filename.to_string()))
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

    /// Submit an already-patched workflow. Returns ComfyUI's prompt id.
    pub async fn submit_prompt(&self, workflow: &Value) -> anyhow::Result<String> {
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

    /// Fetch history for a prompt. `Ok(None)` means "not executed yet"
    /// (ComfyUI returns `{}` until the entry materialises). `Ok(Some(...))`
    /// means the entry exists — caller should check `status.status_str`.
    pub async fn get_history(&self, prompt_id: &str) -> anyhow::Result<Option<HistoryEntry>> {
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

    /// Lightweight liveness probe. Succeeds if ComfyUI answers `/system_stats`
    /// with a 2xx. Used by the background health monitor.
    pub async fn health(&self) -> anyhow::Result<()> {
        self.http
            .get(format!("{}/system_stats", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Download a produced output (image, mask, etc). Bytes are whatever
    /// content-type ComfyUI serves — typically `image/png` for FLUX outputs.
    pub async fn view(
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
}
