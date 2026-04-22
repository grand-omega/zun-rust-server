use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind_addr: String,
}

impl Config {
    /// Load config from environment variables with dev-friendly defaults.
    /// TOML config file support comes later; env is sufficient for now.
    pub fn from_env() -> Self {
        let data_dir = std::env::var("ZUN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));
        let bind_addr = std::env::var("ZUN_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        Self {
            data_dir,
            bind_addr,
        }
    }
}
