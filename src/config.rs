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
    /// ComfyUI's own data directory (the one holding `input/` and
    /// `output/`), when it runs on this machine. Optional and unset by
    /// default.
    ///
    /// ComfyUI never prunes those two directories and exposes no HTTP
    /// endpoint to delete from them, so every job leaves an uploaded input
    /// and a second copy of its output behind permanently — while
    /// `purge_after_days` only ever governed this server's own copies. Point
    /// this at ComfyUI and the purge task cleans up after itself there too,
    /// touching nothing but the `zun_`-prefixed files it created.
    #[serde(default)]
    pub comfy_data_dir: Option<PathBuf>,
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
        if config.token.len() < 32 {
            anyhow::bail!("token must be at least 32 characters");
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        if config.data_dir.is_relative() {
            config.data_dir = base.join(&config.data_dir);
        }
        if let Some(dir) = config.comfy_data_dir.as_ref()
            && dir.is_relative()
        {
            config.comfy_data_dir = Some(base.join(dir));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, token: &str) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, format!("token = \"{token}\"\n")).unwrap();
        path
    }

    #[test]
    fn rejects_token_shorter_than_32_chars() {
        let dir = tempfile::tempdir().unwrap();
        // 31 chars — one short of the minimum.
        let path = write_config(dir.path(), &"a".repeat(31));
        let err = Config::load(&path).unwrap_err();
        assert!(
            err.to_string().contains("at least 32 characters"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_token_at_32_chars() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), &"a".repeat(32));
        let config = Config::load(&path).unwrap();
        assert_eq!(config.token.len(), 32);
    }
}
