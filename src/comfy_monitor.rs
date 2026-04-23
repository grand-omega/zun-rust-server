//! Background probe that keeps a view of ComfyUI's reachability fresh.
//!
//! Motivation: ComfyUI is a separate process and can crash, restart, or
//! become unresponsive. Without a dedicated liveness channel, the server
//! only learns about it when a job fails — and an individual job failure
//! is indistinguishable from an actual ComfyUI crash. A periodic probe
//! lets us (a) report status via `/api/health` so the Android app can
//! surface a banner, and (b) emit a single loud audit event on the
//! healthy → unhealthy transition so operators learn about crashes
//! immediately rather than after a user submits.
//!
//! Logging discipline:
//! - Transition healthy → unhealthy: ERROR `comfy.unreachable` (once).
//! - Transition unhealthy → healthy: INFO  `comfy.recovered` (once).
//! - Steady-state unhealthy: WARN `comfy.still_unreachable` every ~5 min
//!   (current cadence = every 10 probes @ 30s = 5 min). Avoids log spam.

use std::{sync::Arc, time::Duration};

use tokio::sync::{RwLock, watch};

use crate::comfy::ComfyClient;

const PROBE_INTERVAL: Duration = Duration::from_secs(30);
/// How many consecutive failures between "still unreachable" reminders.
const REMIND_EVERY: u32 = 10;

#[derive(Debug, Clone, Default)]
pub struct ComfyHealth {
    /// Unix seconds of the last probe that returned 2xx. None if we've
    /// never seen ComfyUI healthy since startup.
    pub last_ok_at: Option<i64>,
    /// Consecutive probe failures — resets to 0 on the next success.
    pub consecutive_failures: u32,
    /// Rendered error chain from the most recent failed probe.
    pub last_error: Option<String>,
}

impl ComfyHealth {
    pub fn is_healthy(&self) -> bool {
        self.consecutive_failures == 0 && self.last_ok_at.is_some()
    }
}

pub type ComfyHealthHandle = Arc<RwLock<ComfyHealth>>;

pub fn new_handle() -> ComfyHealthHandle {
    Arc::new(RwLock::new(ComfyHealth::default()))
}

/// Spawn the monitor. Runs until `shutdown` flips to true.
pub fn spawn(
    comfy: ComfyClient,
    handle: ComfyHealthHandle,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            probe_once(&comfy, &handle).await;
            tokio::select! {
                _ = tokio::time::sleep(PROBE_INTERVAL) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::debug!("comfy monitor stopping");
                        return;
                    }
                }
            }
        }
    })
}

async fn probe_once(comfy: &ComfyClient, handle: &ComfyHealthHandle) {
    let now = chrono::Utc::now().timestamp();
    match comfy.health().await {
        Ok(()) => {
            let was_unhealthy = {
                let mut h = handle.write().await;
                let was = h.consecutive_failures > 0 || h.last_ok_at.is_none();
                h.last_ok_at = Some(now);
                h.consecutive_failures = 0;
                h.last_error = None;
                was
            };
            if was_unhealthy {
                tracing::info!(
                    target: "audit",
                    event = "comfy.recovered",
                    last_ok_at = now,
                    "ComfyUI reachable again",
                );
            }
        }
        Err(e) => {
            let (consecutive, was_healthy) = {
                let mut h = handle.write().await;
                let was_healthy = h.consecutive_failures == 0;
                h.consecutive_failures += 1;
                h.last_error = Some(format!("{e:#}"));
                (h.consecutive_failures, was_healthy)
            };
            if was_healthy {
                tracing::error!(
                    target: "audit",
                    event = "comfy.unreachable",
                    error = ?e,
                    "ComfyUI became unreachable",
                );
            } else if consecutive.is_multiple_of(REMIND_EVERY) {
                tracing::warn!(
                    target: "audit",
                    event = "comfy.still_unreachable",
                    consecutive_failures = consecutive,
                    error = ?e,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as mock_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn probe_marks_healthy_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/system_stats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let comfy = ComfyClient::new(server.uri()).unwrap();
        let handle = new_handle();
        probe_once(&comfy, &handle).await;

        let h = handle.read().await;
        assert!(h.is_healthy());
        assert_eq!(h.consecutive_failures, 0);
        assert!(h.last_ok_at.is_some());
        assert!(h.last_error.is_none());
    }

    #[tokio::test]
    async fn probe_marks_unhealthy_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/system_stats"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let comfy = ComfyClient::new(server.uri()).unwrap();
        let handle = new_handle();
        probe_once(&comfy, &handle).await;

        let h = handle.read().await;
        assert!(!h.is_healthy());
        assert_eq!(h.consecutive_failures, 1);
        assert!(h.last_error.is_some());
    }

    #[tokio::test]
    async fn consecutive_failures_increment() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(mock_path("/system_stats"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let comfy = ComfyClient::new(server.uri()).unwrap();
        let handle = new_handle();
        for _ in 0..3 {
            probe_once(&comfy, &handle).await;
        }
        assert_eq!(handle.read().await.consecutive_failures, 3);
    }
}
