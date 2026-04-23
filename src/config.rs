use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind_addr: String,
    pub token: String,
    pub comfy_url: String,
    /// Absolute or relative path to prompts.yaml. Defaults to
    /// `{data_dir}/prompts.yaml`; override with `ZUN_PROMPTS_PATH` to put
    /// real production prompts outside the repo (and out of git / Claude's
    /// working directory).
    pub prompts_path: PathBuf,
}

impl Config {
    /// Load config from environment variables with dev-friendly defaults.
    /// `ZUN_TOKEN` is required; everything else has a default.
    /// TOML config file support comes later; env is sufficient for now.
    pub fn from_env() -> anyhow::Result<Self> {
        let data_dir = std::env::var("ZUN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));
        let bind_addr = std::env::var("ZUN_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let token = std::env::var("ZUN_TOKEN")
            .map_err(|_| anyhow::anyhow!("ZUN_TOKEN env var is required"))?;
        if token.len() < 16 {
            anyhow::bail!("ZUN_TOKEN must be at least 16 characters");
        }
        let comfy_url =
            std::env::var("ZUN_COMFY_URL").unwrap_or_else(|_| "http://127.0.0.1:8188".to_string());
        let prompts_path = std::env::var("ZUN_PROMPTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("prompts.yaml"));
        Ok(Self {
            data_dir,
            bind_addr,
            token,
            comfy_url,
            prompts_path,
        })
    }
}
