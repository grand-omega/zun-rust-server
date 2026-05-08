use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
};

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use serde::{Deserialize, Deserializer};

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
    /// IPs / CIDR ranges of reverse proxies whose `X-Forwarded-For` we
    /// trust. The peer IP of an incoming connection is checked against
    /// this list; if any entry contains it, the leftmost XFF hop is used
    /// as the real client IP for auth-failure rate limiting and audit
    /// logs. Empty list means "no proxy in front" — the raw TCP peer is
    /// always used.
    ///
    /// Each entry is a string parsed as either a plain IP (`"127.0.0.1"`,
    /// implicitly /32 or /128) or CIDR (`"100.64.0.0/10"` for a tailnet,
    /// `"172.16.0.0/12"` for a Docker bridge). Mixing both forms is fine.
    #[serde(
        default = "default_trusted_proxies",
        deserialize_with = "deserialize_trusted_proxies"
    )]
    pub trusted_proxies: Vec<IpNet>,
}

fn default_bind() -> String {
    "127.0.0.1:8080".into()
}
fn default_trusted_proxies() -> Vec<IpNet> {
    vec![
        IpNet::V4(Ipv4Net::from(Ipv4Addr::LOCALHOST)),
        IpNet::V6(Ipv6Net::from(Ipv6Addr::LOCALHOST)),
    ]
}

/// Accepts both `"127.0.0.1"` (bare IP, treated as /32 or /128) and
/// `"100.64.0.0/10"` (CIDR). The bare-IP form keeps the simple case
/// readable in `config.toml`; the CIDR form covers tailnets, Docker
/// bridges, and other ranges where the proxy IP is dynamic.
fn deserialize_trusted_proxies<'de, D>(deserializer: D) -> Result<Vec<IpNet>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Vec<String> = Vec::deserialize(deserializer)?;
    raw.into_iter()
        .map(|s| {
            s.parse::<IpNet>()
                .or_else(|_| s.parse::<IpAddr>().map(ip_to_net))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}

fn ip_to_net(ip: IpAddr) -> IpNet {
    match ip {
        IpAddr::V4(v) => IpNet::V4(Ipv4Net::from(v)),
        IpAddr::V6(v) => IpNet::V6(Ipv6Net::from(v)),
    }
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
