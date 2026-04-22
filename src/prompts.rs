use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

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
    let parsed: PromptsFile = serde_yaml::from_str(&raw)?;
    let mut map = HashMap::with_capacity(parsed.prompts.len());
    for p in parsed.prompts {
        if map.contains_key(&p.id) {
            anyhow::bail!("duplicate prompt id: {}", p.id);
        }
        map.insert(p.id.clone(), p);
    }
    Ok(map)
}
