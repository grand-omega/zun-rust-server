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
    /// Days before soft-deleted jobs (and unused input cache files) are
    /// hard-purged. Mirrors the app's undo window.
    #[serde(default = "default_retention_days")]
    pub purge_after_days: u32,
    /// Days of daily database backups to keep in `data/backups/`.
    #[serde(default = "default_retention_days")]
    pub backup_keep_days: u32,
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
fn default_retention_days() -> u32 {
    30
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
    /// Read a config from disk and resolve relative paths against the
    /// config file's parent directory. The latter is what makes
    /// `cargo install`'d binaries work from arbitrary CWDs: a value like
    /// `data_dir = "./data"` consistently means "next to config.toml",
    /// not "next to wherever the user happens to be `cd`'d."
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(missing_config_message(path))
            } else {
                anyhow::anyhow!("cannot read {}: {}", path.display(), e)
            }
        })?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("invalid {}: {}", path.display(), e))?;
        if config.token == "REPLACE_ME_RUN_JUST_SETUP" {
            anyhow::bail!(
                "token is still the example placeholder; run `just setup` to generate a real one"
            );
        }
        if config.token.len() < 16 {
            anyhow::bail!("token must be at least 16 characters");
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        if config.data_dir.is_relative() {
            config.data_dir = base.join(&config.data_dir);
        }
        Ok(config)
    }
}

fn missing_config_message(path: &Path) -> String {
    format!(
        "config file not found at {p}\n\
         to create one:\n  \
           cp config.example.toml config.toml\n  \
           # then edit it: set `token`\n\
         or supply an explicit path:\n  \
           zun-rust-server --config /path/to/config.toml",
        p = path.display(),
    )
}
