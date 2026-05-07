use std::sync::Arc;

use parking_lot::Mutex;
use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot};

use crate::{
    Config, auth::AuthLimiter, comfy::ComfyClient, comfy_monitor::ComfyHealthHandle,
    workflow::WorkflowRegistry,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    /// Workflow templates plus server support status, admin-curated from
    /// `data/workflows/*.json`.
    pub workflows: Arc<WorkflowRegistry>,
    pub comfy: ComfyClient,
    /// Latest known ComfyUI reachability; updated by the monitor task,
    /// read by `/api/v1/health`.
    pub comfy_health: ComfyHealthHandle,
    /// One-slot channel used by the submit handler to wake the worker
    /// when a new job is inserted. `try_send` is always used — filling
    /// the channel means the worker already has one wake pending.
    pub worker_tx: mpsc::Sender<()>,
    /// Per-IP sliding-window limiter for failed auth attempts.
    pub auth_limiter: AuthLimiter,
    /// Cached disk-usage measurement for `/health`. Walking the data dir
    /// is fast on a personal box but we don't want every health probe to
    /// trigger it; cache the result for ~60s.
    pub disk_usage_cache: Arc<Mutex<Option<DiskUsageSample>>>,
    /// Cancellation handle for an in-flight Diffusers (`flux2_klein_9b_kv_*`)
    /// subprocess. The ComfyUI path is stopped via `comfy.interrupt()`; the
    /// Diffusers path runs an external python process that needs an
    /// out-of-band kill signal. At most one entry exists at a time because
    /// the worker is single-threaded.
    pub running_diffusers_cancel: Arc<Mutex<Option<DiffusersCancel>>>,
}

#[derive(Debug, Clone, Copy)]
pub struct DiskUsageSample {
    pub total_bytes: u64,
    pub computed_at: std::time::Instant,
}

#[derive(Debug)]
pub struct DiffusersCancel {
    pub job_id: String,
    pub tx: oneshot::Sender<()>,
}
