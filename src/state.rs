use std::{collections::HashMap, sync::Arc};

use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::{Config, comfy::ComfyClient, comfy_monitor::ComfyHealthHandle, prompts::Prompt};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    pub prompts: Arc<HashMap<String, Prompt>>,
    pub workflows: Arc<HashMap<String, serde_json::Value>>,
    pub comfy: ComfyClient,
    /// Latest known ComfyUI reachability; updated by the monitor task,
    /// read by `/api/health`.
    pub comfy_health: ComfyHealthHandle,
    /// One-slot channel used by the submit handler to wake the worker
    /// when a new job is inserted. `try_send` is always used — filling
    /// the channel means the worker already has one wake pending.
    pub worker_tx: mpsc::Sender<()>,
}
