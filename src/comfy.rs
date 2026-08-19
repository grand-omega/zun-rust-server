//! HTTP + WebSocket client for the ComfyUI API used by project-zun.
//!
//! Speaks four HTTP endpoints and one WebSocket:
//! - `POST /upload/image` — push an input image, get back the name ComfyUI stored it under.
//! - `POST /prompt` — submit a patched workflow JSON (with `client_id`), get back a `prompt_id`.
//! - `GET  /history/{prompt_id}` — fetch outputs once the prompt has finished.
//! - `GET  /view?filename&subfolder&type` — download an output image.
//! - `ws   /ws?clientId={id}` — completion/error events for prompts tagged with `client_id`.
//!
//! The worker opens a fresh ws per job, submits with the matching `client_id`,
//! and waits for the terminal event. `/history` is then fetched once for the
//! structured outputs payload. The per-job timeout lives in the worker.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

/// WebSocket stream used to receive completion events from ComfyUI.
pub type ComfyWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Thin wrapper over `reqwest::Client` with a fixed ComfyUI base URL.
#[derive(Clone)]
pub struct ComfyClient {
    base: String,
    /// Base for `/ws` connections. Derived from `base` by swapping scheme
    /// (`http`→`ws`, `https`→`wss`). Stored separately so integration
    /// tests can point the ws at a different port than the HTTP mock.
    ws_base: String,
    /// Client used for job-related traffic; generous 60s timeout since
    /// /upload/image and /view can ship MBs.
    http: Client,
}

/// Number of attempts (including the first) for idempotent ComfyUI calls.
const MAX_ATTEMPTS: u32 = 3;
/// Base backoff applied as `BASE << (attempt - 1)` between retries.
const BASE_BACKOFF: Duration = Duration::from_millis(500);
/// Ceiling on a buffered JSON response body (`/upload/image`, `/prompt`,
/// `/history`). ComfyUI is operator-configured and therefore trusted, but a
/// backend bug returning an unbounded body would OOM the only process on the
/// box — reqwest has no built-in cap, so impose one here.
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
/// Same ceiling for `/view` image downloads. Generous next to a FLUX2 PNG
/// (single-digit MB) while still bounded.
const MAX_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// How long to wait for the WebSocket TCP connect + upgrade handshake
/// before giving up. Without this, a wedged backend that accepts the TCP
/// connection but never
/// completes the upgrade would stall the worker (which processes one job
/// at a time) indefinitely.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Buffer a response body, aborting once it exceeds `cap`. Replaces
/// `.bytes()`/`.json()`, neither of which bounds the body it collects.
async fn read_capped(resp: reqwest::Response, cap: usize, what: &str) -> anyhow::Result<Vec<u8>> {
    if let Some(len) = resp.content_length()
        && len > cap as u64
    {
        anyhow::bail!("{what}: declared body of {len} bytes exceeds the {cap}-byte cap");
    }
    let mut resp = resp;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() + chunk.len() > cap {
            anyhow::bail!("{what}: body exceeded the {cap}-byte cap mid-stream");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// `read_capped` + JSON decode, for the endpoints that return JSON.
async fn json_capped(resp: reqwest::Response, what: &str) -> anyhow::Result<Value> {
    let bytes = read_capped(resp, MAX_JSON_BYTES, what).await?;
    serde_json::from_slice(&bytes).map_err(|e| anyhow::anyhow!("{what}: invalid json: {e}"))
}

impl ComfyClient {
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        let base = base.into();
        let ws_base = derive_ws_base(&base);
        Self::build(base, ws_base)
    }

    /// Construct with an explicit ws base — for integration tests that
    /// run the ws mock on a different port than the HTTP mock.
    pub fn with_ws_base(
        base: impl Into<String>,
        ws_base: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::build(base.into(), ws_base.into())
    }

    fn build(base: String, ws_base: String) -> anyhow::Result<Self> {
        let http = Client::builder().timeout(Duration::from_secs(60)).build()?;
        Ok(Self {
            base,
            ws_base,
            http,
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
        let resp = self
            .http
            .post(format!("{}/upload/image", self.base))
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;
        let resp = json_capped(resp, "upload_image").await?;
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

    async fn submit_prompt_once(
        &self,
        workflow: &Value,
        client_id: &str,
    ) -> anyhow::Result<String> {
        let resp = self
            .http
            .post(format!("{}/prompt", self.base))
            .json(&serde_json::json!({ "prompt": workflow, "client_id": client_id }))
            .send()
            .await?
            .error_for_status()?;
        let resp = json_capped(resp, "submit_prompt").await?;
        resp["prompt_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("submit_prompt response missing `prompt_id`: {resp}"))
    }

    /// Submit an already-patched workflow. `client_id` tags the prompt so
    /// events for it are routed to the ws connection opened with the same id.
    /// Returns ComfyUI's prompt id.
    pub async fn submit_prompt(&self, workflow: &Value, client_id: &str) -> anyhow::Result<String> {
        // ConnectOnly: a retry after a timeout mid-request could duplicate
        // the submission since ComfyUI may have already accepted it.
        with_retry("submit_prompt", Idempotency::ConnectOnly, || {
            self.submit_prompt_once(workflow, client_id)
        })
        .await
    }

    /// Open a WebSocket to ComfyUI's `/ws` endpoint tagged with `client_id`.
    /// Events for prompts submitted with the matching `client_id` will arrive
    /// on this stream until it's closed. Call before `submit_prompt` so no
    /// events are missed between queueing and executing.
    pub async fn connect_ws(&self, client_id: &str) -> anyhow::Result<ComfyWs> {
        self.connect_ws_with_timeout(client_id, WS_CONNECT_TIMEOUT)
            .await
    }

    async fn connect_ws_with_timeout(
        &self,
        client_id: &str,
        timeout: Duration,
    ) -> anyhow::Result<ComfyWs> {
        let url = format!("{}/ws?clientId={client_id}", self.ws_base);
        let (ws, _resp) = tokio::time::timeout(timeout, connect_async(&url))
            .await
            .map_err(|_| anyhow::anyhow!("connect comfy ws {url}: timed out after {timeout:?}"))?
            .map_err(|e| anyhow::anyhow!("connect comfy ws {url}: {e}"))?;
        Ok(ws)
    }

    async fn get_history_once(&self, prompt_id: &str) -> anyhow::Result<Option<HistoryEntry>> {
        let resp = self
            .http
            .get(format!("{}/history/{prompt_id}", self.base))
            .send()
            .await?
            .error_for_status()?;
        let resp = json_capped(resp, "get_history").await?;
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

    /// Cancel ComfyUI's currently-executing prompt. Idempotent: if nothing
    /// is running, ComfyUI 200s anyway. Used by the cancel handler when a
    /// user wants to stop a job that's already running on the GPU.
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        self.http
            .post(format!("{}/interrupt", self.base))
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
        read_capped(resp, MAX_IMAGE_BYTES, "view").await
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

fn derive_ws_base(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    }
}

/// Drain ComfyUI ws frames until the given `prompt_id` terminates. Returns
/// `Ok(())` on successful completion (`executing` with `node:null`) and
/// `Err` on `execution_error` or if the stream closes first. Events for
/// other prompt ids are ignored. `progress` fractions (value/max from
/// ComfyUI `progress` frames, clamped to 0..1) are forwarded to the
/// optional sender; send errors are ignored (the receiver may be gone).
pub async fn await_completion(
    ws: &mut ComfyWs,
    prompt_id: &str,
    progress: Option<&tokio::sync::mpsc::UnboundedSender<f32>>,
) -> anyhow::Result<()> {
    while let Some(frame) = ws.next().await {
        let msg = frame.map_err(|e| anyhow::anyhow!("comfy ws error: {e}"))?;
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => anyhow::bail!("comfy ws closed before completion"),
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v["data"]["prompt_id"].as_str() != Some(prompt_id) {
            continue;
        }
        match v["type"].as_str() {
            Some("executing") if v["data"]["node"].is_null() => return Ok(()),
            Some("progress") => {
                if let Some(tx) = progress
                    && let (Some(value), Some(max)) =
                        (v["data"]["value"].as_f64(), v["data"]["max"].as_f64())
                    && max > 0.0
                {
                    let _ = tx.send(((value / max).clamp(0.0, 1.0)) as f32);
                }
            }
            Some("execution_error") => {
                let details = v["data"].to_string();
                anyhow::bail!("comfyui execution_error: {details}");
            }
            _ => {}
        }
    }
    anyhow::bail!("comfy ws stream ended before completion")
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
    async fn read_capped_rejects_a_body_larger_than_the_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4096]))
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = client
            .get(format!("{}/big", server.uri()))
            .send()
            .await
            .unwrap();
        let err = read_capped(resp, 1024, "test").await.unwrap_err();
        assert!(err.to_string().contains("cap"), "got: {err}");
    }

    #[tokio::test]
    async fn read_capped_returns_a_body_within_the_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/small"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = client
            .get(format!("{}/small", server.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(read_capped(resp, 1024, "test").await.unwrap(), b"hello");
    }

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
        let pid = client.submit_prompt(&wf, "test-client").await.unwrap();
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

    // ---- ws helpers ----

    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// Start a ws server on 127.0.0.1:<ephemeral> that accepts one
    /// connection, sends each frame in `frames`, then closes. Returns the
    /// `ws://` URL the client should connect to.
    async fn start_ws_server(frames: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            for f in frames {
                ws.send(Message::text(f)).await.unwrap();
            }
            let _ = ws.close(None).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn await_completion_returns_on_executing_node_null() {
        let frames = vec![
            // An event for a different prompt id — should be ignored.
            r#"{"type":"executing","data":{"node":"1","prompt_id":"other"}}"#.to_string(),
            // Progress/status without prompt_id — should be ignored.
            r#"{"type":"status","data":{"status":{"exec_info":{"queue_remaining":1}}}}"#
                .to_string(),
            // Mid-execution for our prompt — not terminal yet.
            r#"{"type":"executing","data":{"node":"3","prompt_id":"mine"}}"#.to_string(),
            // Terminal event for our prompt.
            r#"{"type":"executing","data":{"node":null,"prompt_id":"mine"}}"#.to_string(),
        ];
        let base = start_ws_server(frames).await;
        let client = ComfyClient::new(&base).unwrap();
        let mut ws = client.connect_ws("test-client").await.unwrap();
        await_completion(&mut ws, "mine", None).await.unwrap();
    }

    #[tokio::test]
    async fn await_completion_forwards_progress_fractions() {
        let frames = vec![
            // Progress for another prompt — ignored.
            r#"{"type":"progress","data":{"value":1,"max":10,"prompt_id":"other"}}"#.to_string(),
            r#"{"type":"progress","data":{"value":5,"max":20,"prompt_id":"mine"}}"#.to_string(),
            // max=0 — ignored (no division by zero).
            r#"{"type":"progress","data":{"value":1,"max":0,"prompt_id":"mine"}}"#.to_string(),
            r#"{"type":"progress","data":{"value":20,"max":20,"prompt_id":"mine"}}"#.to_string(),
            r#"{"type":"executing","data":{"node":null,"prompt_id":"mine"}}"#.to_string(),
        ];
        let base = start_ws_server(frames).await;
        let client = ComfyClient::new(&base).unwrap();
        let mut ws = client.connect_ws("test-client").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        await_completion(&mut ws, "mine", Some(&tx)).await.unwrap();
        drop(tx);
        let mut got = Vec::new();
        while let Some(p) = rx.recv().await {
            got.push(p);
        }
        assert_eq!(got, vec![0.25, 1.0]);
    }

    #[tokio::test]
    async fn await_completion_errors_on_execution_error() {
        let frames = vec![
            r#"{"type":"execution_error","data":{"prompt_id":"mine","exception_message":"boom"}}"#
                .to_string(),
        ];
        let base = start_ws_server(frames).await;
        let client = ComfyClient::new(&base).unwrap();
        let mut ws = client.connect_ws("test-client").await.unwrap();
        let err = await_completion(&mut ws, "mine", None).await.unwrap_err();
        assert!(format!("{err}").contains("execution_error"));
    }

    #[tokio::test]
    async fn await_completion_errors_on_stream_close_before_terminal() {
        let frames =
            vec![r#"{"type":"executing","data":{"node":"1","prompt_id":"mine"}}"#.to_string()];
        let base = start_ws_server(frames).await;
        let client = ComfyClient::new(&base).unwrap();
        let mut ws = client.connect_ws("test-client").await.unwrap();
        let err = await_completion(&mut ws, "mine", None).await.unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("closed") || s.contains("ended"), "got: {s}");
    }

    #[tokio::test]
    async fn connect_ws_times_out_if_backend_never_completes_upgrade() {
        // Simulate a wedged ComfyUI: accept the TCP connection but never
        // respond to the WS upgrade handshake.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _stream = listener.accept().await.unwrap();
            std::future::pending::<()>().await
        });

        let client = ComfyClient::new(format!("http://{addr}")).unwrap();
        let err = client
            .connect_ws_with_timeout("test-client", Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("timed out"), "got: {err}");
    }

    #[test]
    fn derive_ws_base_swaps_scheme() {
        assert_eq!(
            derive_ws_base("http://127.0.0.1:8188"),
            "ws://127.0.0.1:8188"
        );
        assert_eq!(
            derive_ws_base("https://example.com:8188"),
            "wss://example.com:8188"
        );
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
