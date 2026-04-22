use std::{collections::HashMap, sync::Arc};

use sqlx::SqlitePool;

use crate::{Config, prompts::Prompt};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Config,
    pub prompts: Arc<HashMap<String, Prompt>>,
    pub workflows: Arc<HashMap<String, serde_json::Value>>,
}
