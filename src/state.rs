use std::sync::Arc;

use parking_lot::Mutex;
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::{Config, comfy::ComfyClient, workflow::WorkflowRegistry};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    /// Workflow templates plus server support status, admin-curated from
    /// `data/workflows/*.json`.
    pub workflows: Arc<WorkflowRegistry>,
    pub comfy: ComfyClient,
    /// One-slot channel used by the submit handler to wake the worker
    /// when a new job is inserted. `try_send` is always used — filling
    /// the channel means the worker already has one wake pending.
    pub worker_tx: mpsc::Sender<()>,
    /// Bumped whenever a job's status or progress changes, so a long-poll
    /// (`GET /jobs/{id}?wait=`) wakes the moment the worker writes instead
    /// of rediscovering it on the next timer tick. A plain counter rather
    /// than per-job channels: a waiter that wakes for someone else's job
    /// just re-reads its row and goes back to sleep, which is cheaper than
    /// tracking subscriptions.
    pub job_events: Arc<tokio::sync::watch::Sender<u64>>,
    /// Cached disk-usage measurement for `/health`. Walking the data dir
    /// is fast on a personal box but we don't want every health probe to
    /// trigger it; cache the result for ~60s.
    pub disk_usage_cache: Arc<Mutex<Option<DiskUsageSample>>>,
}

#[derive(Debug, Clone, Copy)]
pub struct DiskUsageSample {
    pub total_bytes: u64,
    pub computed_at: std::time::Instant,
}
