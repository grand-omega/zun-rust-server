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
    /// Optional override directory holding the ComfyUI workflow templates
    /// (`*.json`). When unset, the templates baked into the binary at
    /// build time (`workflows/` in the repo root) are used. Set this only
    /// to point at an on-disk copy during workflow authoring.
    #[serde(default)]
    pub workflows_dir: Option<PathBuf>,
    /// Workflow selected by default in capability responses.
    #[serde(default = "default_workflow")]
    pub default_workflow: String,
    /// Explicit list of workflow names this server exposes.
    #[serde(default = "default_enabled_workflows")]
    pub enabled_workflows: Vec<String>,
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
fn default_workflow() -> String {
    "flux2_klein_edit".into()
}
fn default_enabled_workflows() -> Vec<String> {
    vec!["flux2_klein_edit".into()]
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
}
