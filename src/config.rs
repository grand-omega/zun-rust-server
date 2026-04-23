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
    #[serde(default)]
    pub log_format: LogFormat,
}

fn default_bind() -> String {
    "127.0.0.1:8080".into()
}
fn default_comfy_url() -> String {
    "http://127.0.0.1:8188".into()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
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

    fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
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
}
