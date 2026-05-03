use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub token: String,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_comfy_url")]
    pub comfy_url: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Directory holding the ComfyUI workflow templates (`*.json`).
    /// Defaults to `<data_dir>/workflows` when unset.
    #[serde(default)]
    pub workflows_dir: Option<PathBuf>,
    /// Workflow selected by default in capability responses.
    #[serde(default = "default_workflow")]
    pub default_workflow: String,
    /// Explicit list of workflow names this server exposes.
    #[serde(default = "default_enabled_workflows")]
    pub enabled_workflows: Vec<String>,
    /// On-disk path to the FLUX2 9B-KV weights, recorded in audit logs and
    /// the per-job sidecar metadata. Required when the experimental
    /// virtual workflow is used; otherwise ignored.
    #[serde(default)]
    pub diffusers_model_path: Option<PathBuf>,
    #[serde(default)]
    pub log_format: LogFormat,
}

fn default_bind() -> String {
    "0.0.0.0:8080".into()
}
fn default_comfy_url() -> String {
    "http://127.0.0.1:8188".into()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}
fn default_workflow() -> String {
    "flux2_klein_edit".into()
}
fn default_enabled_workflows() -> Vec<String> {
    vec![
        "flux2_klein_edit".into(),
        "flux2_klein_9b_kv_experimental".into(),
    ]
}

/// Log output format. Defaults to `auto` (pretty when stderr is a TTY, JSON otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Auto,
    Pretty,
    Json,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        Self::from_file("config.toml")
    }

    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;
        let config: Self = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid {}: {}", path.display(), e))?;
        if config.token.len() < 16 {
            anyhow::bail!("token must be at least 16 characters");
        }
        Ok(config)
    }

    /// Resolved workflow templates directory: explicit `workflows_dir` if set,
    /// otherwise `<data_dir>/workflows`.
    pub fn resolved_workflows_dir(&self) -> PathBuf {
        self.workflows_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("workflows"))
    }
}
