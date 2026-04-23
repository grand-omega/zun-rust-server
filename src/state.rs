use std::{collections::HashMap, sync::Arc};

use sqlx::SqlitePool;
use tokio::sync::mpsc;

use crate::{Config, comfy::ComfyClient, prompts::Prompt};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    pub prompts: Arc<HashMap<String, Prompt>>,
    pub workflows: Arc<HashMap<String, serde_json::Value>>,
    pub comfy: ComfyClient,
    /// One-slot channel used by the submit handler to wake the worker
    /// when a new job is inserted. `try_send` is always used — filling
    /// the channel means the worker already has one wake pending.
    pub worker_tx: mpsc::Sender<()>,
}
