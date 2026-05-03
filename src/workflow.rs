//! ComfyUI workflow template loading and placeholder substitution.
//!
//! Templates are opaque JSON blobs owned by the sibling project-zun repo
//! (see `doc/WORKFLOWS.md` there for the full placeholder contract). This
//! module loads them and performs **whole-string** substitution on known
//! placeholder tokens. Substring matches are intentional non-matches:
//! `PROMPT_PLACEHOLDER` must occupy an entire JSON string value.
use std::{collections::HashMap, path::Path};

use serde::Serialize;
use serde_json::Value;

// --- Placeholder tokens (mirrors project-zun/doc/WORKFLOWS.md). -----------

pub const PROMPT: &str = "PROMPT_PLACEHOLDER";
pub const INPUT_IMAGE: &str = "INPUT_IMAGE_PLACEHOLDER";
pub const FILENAME_PREFIX: &str = "FILENAME_PREFIX_PLACEHOLDER";
/// Per-job random seed. Substituted as a JSON number, not a string —
/// see `patch_seed_placeholder` for the special-cased handling.
pub const SEED: &str = "SEED_PLACEHOLDER";
// The rest are defined for completeness; the server v1 only populates the
// three above (klein_edit path). Fill / ref / LoRA paths come later.
#[allow(dead_code)]
pub const MASK_IMAGE: &str = "MASK_IMAGE_PLACEHOLDER";
#[allow(dead_code)]
pub const REFERENCE_IMAGE: &str = "REFERENCE_IMAGE_PLACEHOLDER";
#[allow(dead_code)]
pub const MASK_PROMPT: &str = "MASK_PROMPT_PLACEHOLDER";
#[allow(dead_code)]
pub const LORA: &str = "LORA_PLACEHOLDER";

pub const FLUX2_KLEIN_9B_KV_EXPERIMENTAL: &str = "flux2_klein_9b_kv_experimental";

const SERVER_REQUIRED_EDIT_PLACEHOLDERS: &[&str] = &[PROMPT, INPUT_IMAGE, FILENAME_PREFIX, SEED];
const SERVER_SUPPORTED_PLACEHOLDERS: &[&str] = SERVER_REQUIRED_EDIT_PLACEHOLDERS;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowSupport {
    pub name: String,
    pub display_name: String,
    pub kind: String,
    pub requires_input_image: bool,
    pub experimental: bool,
    pub default: bool,
    pub runtime: String,
    pub pipeline: Option<String>,
    pub model_path: Option<String>,
    pub dtype: Option<String>,
    pub offload_mode: Option<String>,
    pub default_steps: Option<u32>,
    pub default_width: Option<u32>,
    pub default_height: Option<u32>,
    pub loaded: bool,
    pub supported: bool,
    pub placeholders: Vec<String>,
    pub warning: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowRegistry {
    pub templates: HashMap<String, Value>,
    pub support: HashMap<String, WorkflowSupport>,
}

impl WorkflowRegistry {
    pub fn empty() -> Self {
        Self {
            templates: HashMap::new(),
            support: HashMap::new(),
        }
    }

    pub fn supported_template(&self, name: &str) -> Result<&Value, WorkflowSupportError> {
        match self.support.get(name) {
            Some(s) if s.supported => self
                .templates
                .get(name)
                .ok_or_else(|| WorkflowSupportError::Unknown(name.to_string())),
            Some(s) => Err(WorkflowSupportError::Unsupported {
                name: name.to_string(),
                reason: s.reason.clone().unwrap_or_else(|| "unsupported".into()),
            }),
            None if is_virtual_supported_workflow(name) => Err(WorkflowSupportError::Virtual {
                name: name.to_string(),
            }),
            None => Err(WorkflowSupportError::Unknown(name.to_string())),
        }
    }

    pub fn supported_count(&self) -> usize {
        self.support.values().filter(|s| s.supported).count() + 1
    }

    pub fn support_list(&self) -> Vec<WorkflowSupport> {
        let mut items: Vec<_> = self.support.values().cloned().collect();
        items.push(flux2_klein_9b_kv_experimental_support());
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }

    pub fn supports(&self, name: &str) -> Result<(), WorkflowSupportError> {
        if is_virtual_supported_workflow(name) {
            return Ok(());
        }
        self.supported_template(name).map(|_| ())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowSupportError {
    #[error("unknown workflow: {0}")]
    Unknown(String),
    #[error("workflow '{name}' is not supported by this server: {reason}")]
    Unsupported { name: String, reason: String },
    #[error("workflow '{name}' is not a ComfyUI template")]
    Virtual { name: String },
}

/// Recursively walk the JSON value; whenever we find a string that is
/// exactly equal to any `needle` in `subs`, replace it with the matching
/// `replacement`. Partial matches are not substituted.
pub fn patch_placeholders(value: &mut Value, subs: &[(&str, &str)]) {
    match value {
        Value::String(s) => {
            for (needle, replacement) in subs {
                if s == *needle {
                    *s = (*replacement).to_string();
                    break;
                }
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                patch_placeholders(v, subs);
            }
        }
        Value::Object(obj) => {
            for v in obj.values_mut() {
                patch_placeholders(v, subs);
            }
        }
        _ => {}
    }
}

/// Build an edit-flow workflow (klein_edit / klein_ref_edit / fill_*).
/// Supplies the four placeholders every edit workflow needs (prompt,
/// input image, filename prefix, seed). Extra workflow-specific placeholders
/// (mask prompt, reference image, lora) are the caller's responsibility via
/// `patch_placeholders` directly.
pub fn build_edit_workflow(
    template: &Value,
    prompt_text: &str,
    input_image_name: &str,
    job_id: &str,
    seed: i64,
) -> Value {
    let mut out = template.clone();
    let prefix = format!("zun_{job_id}");
    patch_placeholders(
        &mut out,
        &[
            (PROMPT, prompt_text),
            (INPUT_IMAGE, input_image_name),
            (FILENAME_PREFIX, &prefix),
        ],
    );
    patch_seed_placeholder(&mut out, seed);
    out
}

pub fn load_registry(dir: &Path) -> anyhow::Result<WorkflowRegistry> {
    let templates = load_templates(dir)?;
    let support = analyze_templates(&templates);
    Ok(WorkflowRegistry { templates, support })
}

pub fn analyze_templates(templates: &HashMap<String, Value>) -> HashMap<String, WorkflowSupport> {
    templates
        .iter()
        .map(|(name, template)| {
            let placeholders = collect_placeholders(template);
            let reason = unsupported_reason(name, &placeholders);
            let supported = reason.is_none();
            let metadata = workflow_metadata(name);
            (
                name.clone(),
                WorkflowSupport {
                    name: name.clone(),
                    display_name: metadata.display_name,
                    kind: metadata.kind.to_string(),
                    requires_input_image: metadata.requires_input_image,
                    experimental: metadata.experimental,
                    default: metadata.default,
                    runtime: metadata.runtime.to_string(),
                    pipeline: metadata.pipeline.map(str::to_string),
                    model_path: metadata.model_path.map(str::to_string),
                    dtype: metadata.dtype.map(str::to_string),
                    offload_mode: metadata.offload_mode.map(str::to_string),
                    default_steps: metadata.default_steps,
                    default_width: metadata.default_width,
                    default_height: metadata.default_height,
                    loaded: true,
                    supported,
                    placeholders,
                    warning: metadata.warning.map(str::to_string),
                    reason,
                },
            )
        })
        .collect()
}

struct WorkflowMetadata {
    display_name: String,
    kind: &'static str,
    requires_input_image: bool,
    experimental: bool,
    default: bool,
    runtime: &'static str,
    pipeline: Option<&'static str>,
    model_path: Option<&'static str>,
    dtype: Option<&'static str>,
    offload_mode: Option<&'static str>,
    default_steps: Option<u32>,
    default_width: Option<u32>,
    default_height: Option<u32>,
    warning: Option<&'static str>,
}

fn workflow_metadata(name: &str) -> WorkflowMetadata {
    match name {
        "flux2_klein_edit" => WorkflowMetadata {
            display_name: "FLUX 2 klein".to_string(),
            kind: "image_edit",
            requires_input_image: true,
            experimental: false,
            default: true,
            runtime: "comfyui",
            pipeline: None,
            model_path: None,
            dtype: None,
            offload_mode: None,
            default_steps: None,
            default_width: None,
            default_height: None,
            warning: None,
        },
        "flux2_klein_9b_kv_edit" => WorkflowMetadata {
            display_name: "FLUX 2 klein 9B-KV".to_string(),
            kind: "image_edit",
            requires_input_image: true,
            experimental: true,
            default: false,
            runtime: "comfyui",
            pipeline: None,
            model_path: None,
            dtype: None,
            offload_mode: None,
            default_steps: None,
            default_width: None,
            default_height: None,
            warning: Some("Experimental heavier workflow; may OOM on 16 GB VRAM."),
        },
        _ => WorkflowMetadata {
            display_name: humanize_workflow_name(name),
            kind: "image_edit",
            requires_input_image: true,
            experimental: false,
            default: false,
            runtime: "comfyui",
            pipeline: None,
            model_path: None,
            dtype: None,
            offload_mode: None,
            default_steps: None,
            default_width: None,
            default_height: None,
            warning: None,
        },
    }
}

pub fn is_virtual_supported_workflow(name: &str) -> bool {
    name == FLUX2_KLEIN_9B_KV_EXPERIMENTAL
}

fn flux2_klein_9b_kv_experimental_support() -> WorkflowSupport {
    WorkflowSupport {
        name: FLUX2_KLEIN_9B_KV_EXPERIMENTAL.to_string(),
        display_name: "FLUX 2 klein 9B-KV Experimental".to_string(),
        kind: "image_edit".to_string(),
        requires_input_image: true,
        experimental: true,
        default: false,
        runtime: "diffusers".to_string(),
        pipeline: Some("Flux2KleinKVPipeline".to_string()),
        model_path: Some("/home/doremy/ml/t2i/flux2-klein-9b-kv".to_string()),
        dtype: Some("bfloat16".to_string()),
        offload_mode: Some("sequential".to_string()),
        default_steps: Some(4),
        default_width: Some(768),
        default_height: Some(1024),
        loaded: true,
        supported: true,
        placeholders: vec![
            PROMPT.to_string(),
            INPUT_IMAGE.to_string(),
            FILENAME_PREFIX.to_string(),
            SEED.to_string(),
        ],
        warning: Some(
            "Experimental Diffusers workflow; slower/heavier and opt-in. May OOM on 16 GB VRAM."
                .to_string(),
        ),
        reason: None,
    }
}

fn humanize_workflow_name(name: &str) -> String {
    name.split('_')
        .map(|part| match part {
            "flux2" => "FLUX 2".to_string(),
            "kv" => "KV".to_string(),
            "t2i" => "T2I".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect_placeholders(value: &Value) -> Vec<String> {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) if is_known_placeholder(s) => out.push(s.clone()),
            Value::Array(arr) => {
                for v in arr {
                    walk(v, out);
                }
            }
            Value::Object(obj) => {
                for v in obj.values() {
                    walk(v, out);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    walk(value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn unsupported_reason(name: &str, placeholders: &[String]) -> Option<String> {
    if name == "flux2_klein_9b_kv_edit" {
        return Some(
            "use flux2_klein_9b_kv_experimental; backend 9B-KV support is Diffusers-backed"
                .to_string(),
        );
    }
    for required in SERVER_REQUIRED_EDIT_PLACEHOLDERS {
        if !placeholders.iter().any(|p| p == required) {
            return Some(format!("missing required placeholder {required}"));
        }
    }
    let unsupported: Vec<_> = placeholders
        .iter()
        .filter(|p| !SERVER_SUPPORTED_PLACEHOLDERS.contains(&p.as_str()))
        .cloned()
        .collect();
    if unsupported.is_empty() {
        None
    } else {
        Some(format!(
            "requires unsupported placeholder{} {}",
            if unsupported.len() == 1 { "" } else { "s" },
            unsupported.join(", ")
        ))
    }
}

fn is_known_placeholder(s: &str) -> bool {
    matches!(
        s,
        PROMPT
            | INPUT_IMAGE
            | FILENAME_PREFIX
            | SEED
            | MASK_IMAGE
            | REFERENCE_IMAGE
            | MASK_PROMPT
            | LORA
    )
}

/// Substitute `"SEED_PLACEHOLDER"` (a string) with a JSON number.
/// Handled separately because all other placeholders stay as strings;
/// seed is a sampler integer.
pub fn patch_seed_placeholder(value: &mut Value, seed: i64) {
    fn walk(v: &mut Value, seed: i64) {
        match v {
            Value::String(s) if s == SEED => {
                *v = Value::Number(serde_json::Number::from(seed));
            }
            Value::Array(arr) => {
                for x in arr.iter_mut() {
                    walk(x, seed);
                }
            }
            Value::Object(obj) => {
                for x in obj.values_mut() {
                    walk(x, seed);
                }
            }
            _ => {}
        }
    }
    walk(value, seed);
}

/// Load every `*.json` in `dir` into a map keyed by file stem
/// (e.g., `"flux2_klein_edit"`). Fails fast on unreadable or malformed
/// files. Duplicate stems (case-insensitive collisions on case-insensitive
/// filesystems) are not expected and would overwrite silently — trust the
/// filesystem.
pub fn load_templates(dir: &Path) -> anyhow::Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read workflows dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("non-utf8 workflow filename: {}", path.display()))?
            .to_string();
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        out.insert(stem, value);
    }
    Ok(out)
}

// --- tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn substitutes_whole_string_match() {
        let mut v = json!({ "text": "PROMPT_PLACEHOLDER" });
        patch_placeholders(&mut v, &[(PROMPT, "hello")]);
        assert_eq!(v, json!({ "text": "hello" }));
    }

    #[test]
    fn leaves_substring_match_alone() {
        // "a PROMPT_PLACEHOLDER b" is NOT a whole-string match.
        let mut v = json!({ "text": "a PROMPT_PLACEHOLDER b" });
        patch_placeholders(&mut v, &[(PROMPT, "hello")]);
        assert_eq!(v, json!({ "text": "a PROMPT_PLACEHOLDER b" }));
    }

    #[test]
    fn walks_nested_objects_and_arrays() {
        let mut v = json!({
            "6": { "inputs": { "text": "PROMPT_PLACEHOLDER" } },
            "10": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" } },
            "list": ["a", "PROMPT_PLACEHOLDER", { "nested": "PROMPT_PLACEHOLDER" }],
        });
        patch_placeholders(
            &mut v,
            &[(PROMPT, "TEST_PROMPT"), (INPUT_IMAGE, "photo.jpg")],
        );
        assert_eq!(
            v,
            json!({
                "6": { "inputs": { "text": "TEST_PROMPT" } },
                "10": { "inputs": { "image": "photo.jpg" } },
                "list": ["a", "TEST_PROMPT", { "nested": "TEST_PROMPT" }],
            })
        );
    }

    #[test]
    fn unknown_placeholders_are_preserved() {
        let mut v = json!({ "lora": "LORA_PLACEHOLDER" });
        patch_placeholders(&mut v, &[(PROMPT, "x")]);
        assert_eq!(v, json!({ "lora": "LORA_PLACEHOLDER" }));
    }

    #[test]
    fn non_string_values_are_untouched() {
        let mut v = json!({ "steps": 20, "seed": 42, "active": true, "none": null });
        patch_placeholders(&mut v, &[(PROMPT, "x")]);
        assert_eq!(
            v,
            json!({ "steps": 20, "seed": 42, "active": true, "none": null })
        );
    }

    #[test]
    fn build_edit_workflow_patches_expected_fields() {
        // Fixture mirrors the shape of flux2_klein_edit.
        let template = json!({
            "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" } },
            "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } },
            "16": { "inputs": { "noise_seed": "SEED_PLACEHOLDER" } },
            "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" } }
        });
        let built = build_edit_workflow(&template, "anime", "in.jpg", "abc-123", 12345);
        assert_eq!(built["9"]["inputs"]["text"], "anime");
        assert_eq!(built["4"]["inputs"]["image"], "in.jpg");
        assert_eq!(built["19"]["inputs"]["filename_prefix"], "zun_abc-123");
        assert_eq!(built["16"]["inputs"]["noise_seed"], 12345);
    }

    #[test]
    fn build_edit_workflow_does_not_touch_original() {
        let template = json!({ "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } } });
        let _ = build_edit_workflow(&template, "x", "y.jpg", "j", 7);
        assert_eq!(template["9"]["inputs"]["text"], "PROMPT_PLACEHOLDER");
    }

    #[test]
    fn seed_placeholder_becomes_json_number() {
        let mut v = json!({ "noise_seed": "SEED_PLACEHOLDER", "other": "PROMPT_PLACEHOLDER" });
        patch_seed_placeholder(&mut v, 42);
        assert_eq!(v["noise_seed"], 42);
        // Non-seed placeholders are untouched by patch_seed_placeholder.
        assert_eq!(v["other"], "PROMPT_PLACEHOLDER");
    }

    #[test]
    fn comfy_9b_kv_template_is_disabled_in_favor_of_diffusers() {
        let mut templates = HashMap::new();
        templates.insert(
            "flux2_klein_9b_kv_edit".to_string(),
            json!({
                "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" } },
                "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } },
                "16": { "inputs": { "noise_seed": "SEED_PLACEHOLDER" } },
                "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" } }
            }),
        );

        let support = analyze_templates(&templates);
        let wf = support.get("flux2_klein_9b_kv_edit").unwrap();
        assert!(!wf.supported);
        assert_eq!(wf.display_name, "FLUX 2 klein 9B-KV");
        assert!(wf.experimental);
        assert_eq!(wf.runtime, "comfyui");
        assert!(wf.reason.as_deref().unwrap().contains("Diffusers-backed"));
    }

    #[test]
    fn flux2_klein_edit_remains_default() {
        let mut templates = HashMap::new();
        templates.insert(
            "flux2_klein_edit".to_string(),
            json!({
                "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" } },
                "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } },
                "16": { "inputs": { "noise_seed": "SEED_PLACEHOLDER" } },
                "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" } }
            }),
        );

        let support = analyze_templates(&templates);
        let wf = support.get("flux2_klein_edit").unwrap();
        assert!(wf.supported);
        assert_eq!(wf.display_name, "FLUX 2 klein");
        assert!(wf.requires_input_image);
        assert!(!wf.experimental);
        assert!(wf.default);
        assert_eq!(wf.warning, None);
    }

    #[test]
    fn support_list_includes_virtual_diffusers_9b_kv() {
        let registry = WorkflowRegistry::empty();
        assert!(registry.supports(FLUX2_KLEIN_9B_KV_EXPERIMENTAL).is_ok());
        assert_eq!(registry.supported_count(), 1);
        let wf = registry
            .support_list()
            .into_iter()
            .find(|wf| wf.name == FLUX2_KLEIN_9B_KV_EXPERIMENTAL)
            .unwrap();
        assert_eq!(wf.display_name, "FLUX 2 klein 9B-KV Experimental");
        assert_eq!(wf.runtime, "diffusers");
        assert_eq!(wf.pipeline.as_deref(), Some("Flux2KleinKVPipeline"));
        assert_eq!(
            wf.model_path.as_deref(),
            Some("/home/doremy/ml/t2i/flux2-klein-9b-kv")
        );
        assert_eq!(wf.dtype.as_deref(), Some("bfloat16"));
        assert_eq!(wf.offload_mode.as_deref(), Some("sequential"));
        assert_eq!(wf.default_steps, Some(4));
        assert_eq!(wf.default_width, Some(768));
        assert_eq!(wf.default_height, Some(1024));
    }

    #[test]
    fn load_templates_loads_all_real_workflows() {
        // This test asserts the contract with project-zun: when the symlink
        // is present, every file loads cleanly and the known templates
        // appear. Skipped if the symlink isn't set up in the dev env.
        let dir = std::path::Path::new("data/workflows");
        if !dir.exists() {
            eprintln!("data/workflows symlink missing; skipping real-template test");
            return;
        }
        let loaded = load_templates(dir).expect("load templates");
        assert!(
            loaded.contains_key("flux2_klein_edit"),
            "expected flux2_klein_edit in real workflows"
        );
        // Real workflow must still contain a PROMPT placeholder (the
        // contract). If this fails, either the symlink points somewhere
        // unexpected or project-zun's contract changed.
        let body = loaded["flux2_klein_edit"].to_string();
        assert!(body.contains("PROMPT_PLACEHOLDER"));
        assert!(body.contains("INPUT_IMAGE_PLACEHOLDER"));
        assert!(body.contains("FILENAME_PREFIX_PLACEHOLDER"));
    }
}
