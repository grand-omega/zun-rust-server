use std::sync::Arc;

use parking_lot::Mutex;
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::{
    Config, auth::AuthLimiter, comfy::ComfyClient, comfy_monitor::ComfyHealthHandle,
    workflow::WorkflowRegistry,
};

/// Identifies which user owns a row. Mandatory parameter on every
/// data-access function so cross-user access is unrepresentable in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(pub i64);

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    /// Workflow templates plus server support status. Shared across users
    /// and admin-curated from `data/workflows/*.json`.
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
}

#[derive(Debug, Clone, Copy)]
pub struct DiskUsageSample {
    pub total_bytes: u64,
    pub computed_at: std::time::Instant,
}
