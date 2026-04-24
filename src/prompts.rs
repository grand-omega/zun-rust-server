use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

/// Default per-prompt timeout. Suits FLUX2 klein (typical ~7s). Bump
/// per-prompt in prompts.yaml for slower workflows (e.g. FLUX.1 Fill).
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;

/// Reserved prompt ID for free-text custom prompts. Injected at startup;
/// never appears in prompts.yaml.
pub const CUSTOM_PROMPT_ID: &str = "__custom__";

fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

/// One catalog entry. `text` and `workflow` are internal; never returned to
/// clients. The public-facing view is `PromptDto` below.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Prompt {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub text: String,
    pub workflow: String,
    /// Overall per-job timeout (seconds) for the ComfyUI poll loop.
    /// Defaults to DEFAULT_TIMEOUT_SECONDS if omitted.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

/// Client-facing projection: only the fields the Android app needs.
#[derive(Debug, Serialize)]
pub struct PromptDto {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

impl From<&Prompt> for PromptDto {
    fn from(p: &Prompt) -> Self {
        Self {
            id: p.id.clone(),
            label: p.label.clone(),
            description: p.description.clone(),
        }
    }
}

#[derive(Deserialize)]
struct PromptsFile {
    prompts: Vec<Prompt>,
}

/// Load prompts from a YAML file. Returns a map keyed by prompt id. The
/// insertion order of `prompts.yaml` is preserved by iterating the source
/// Vec — callers that need ordered output should iterate the file list,
/// not the HashMap.
pub fn load(path: &Path) -> anyhow::Result<HashMap<String, Prompt>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read prompts file {}: {e}", path.display()))?;
    let parsed: PromptsFile = serde_yaml_ng::from_str(&raw)?;
    let mut map = HashMap::with_capacity(parsed.prompts.len());
    for p in parsed.prompts {
        if map.contains_key(&p.id) {
            anyhow::bail!("duplicate prompt id: {}", p.id);
        }
        map.insert(p.id.clone(), p);
    }
    Ok(map)
}

/// Inject the synthetic `__custom__` entry so clients can submit free-text
/// prompts. `workflow` is the stem of the workflow template to use (must
/// exist in the workflows directory). Idempotent — no-op if already present.
pub fn inject_custom(prompts: &mut HashMap<String, Prompt>, workflow: String) {
    prompts
        .entry(CUSTOM_PROMPT_ID.to_string())
        .or_insert_with(|| Prompt {
            id: CUSTOM_PROMPT_ID.to_string(),
            label: "Custom".to_string(),
            description: Some("Enter your own prompt text".to_string()),
            text: String::new(),
            workflow,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        });
}
