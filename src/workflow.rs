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
            support: HashMap::from([(
                FLUX2_KLEIN_9B_KV_EXPERIMENTAL.to_string(),
                flux2_klein_9b_kv_experimental_support("flux2_klein_edit"),
            )]),
        }
    }

    pub fn supported_template(&self, name: &str) -> Result<&Value, WorkflowSupportError> {
        match self.support.get(name) {
            Some(s) if s.supported && s.runtime == "comfyui" => self
                .templates
                .get(name)
                .ok_or_else(|| WorkflowSupportError::Unknown(name.to_string())),
            Some(s) if s.supported && s.runtime == "diffusers" => {
                Err(WorkflowSupportError::Virtual {
                    name: name.to_string(),
                })
            }
            Some(s) => Err(WorkflowSupportError::Unsupported {
                name: name.to_string(),
                reason: s.reason.clone().unwrap_or_else(|| "unsupported".into()),
            }),
            None => Err(WorkflowSupportError::Unknown(name.to_string())),
        }
    }

    pub fn supported_count(&self) -> usize {
        self.support.values().filter(|s| s.supported).count()
    }

    pub fn support_list(&self) -> Vec<WorkflowSupport> {
        let mut items: Vec<_> = self.support.values().cloned().collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }

    pub fn supports(&self, name: &str) -> Result<(), WorkflowSupportError> {
        match self.support.get(name) {
            Some(s) if s.supported => Ok(()),
            Some(s) => Err(WorkflowSupportError::Unsupported {
                name: name.to_string(),
                reason: s.reason.clone().unwrap_or_else(|| "unsupported".into()),
            }),
            None => Err(WorkflowSupportError::Unknown(name.to_string())),
        }
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

pub fn load_registry(
    dir: &Path,
    enabled_workflows: &[String],
    default_workflow: &str,
) -> anyhow::Result<WorkflowRegistry> {
    let mut templates = HashMap::new();
    let mut support = HashMap::new();

    for name in enabled_workflows {
        if is_virtual_supported_workflow(name) {
            support.insert(
                name.clone(),
                flux2_klein_9b_kv_experimental_support(default_workflow),
            );
            continue;
        }

        let template = load_template(dir, name)?;
        templates.insert(name.clone(), template);
        support.insert(name.clone(), workflow_support(name, default_workflow));
    }

    Ok(WorkflowRegistry { templates, support })
}

pub fn support_for_templates(
    templates: &HashMap<String, Value>,
    default_workflow: &str,
) -> HashMap<String, WorkflowSupport> {
    let mut support: HashMap<String, WorkflowSupport> = templates
        .keys()
        .map(|name| (name.clone(), workflow_support(name, default_workflow)))
        .collect();
    support.insert(
        FLUX2_KLEIN_9B_KV_EXPERIMENTAL.to_string(),
        flux2_klein_9b_kv_experimental_support(default_workflow),
    );
    support
}

struct WorkflowMetadata {
    display_name: String,
    kind: &'static str,
    requires_input_image: bool,
    experimental: bool,
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

fn workflow_support(name: &str, default_workflow: &str) -> WorkflowSupport {
    let metadata = workflow_metadata(name);
    WorkflowSupport {
        name: name.to_string(),
        display_name: metadata.display_name,
        kind: metadata.kind.to_string(),
        requires_input_image: metadata.requires_input_image,
        experimental: metadata.experimental,
        default: name == default_workflow,
        runtime: metadata.runtime.to_string(),
        pipeline: metadata.pipeline.map(str::to_string),
        model_path: metadata.model_path.map(str::to_string),
        dtype: metadata.dtype.map(str::to_string),
        offload_mode: metadata.offload_mode.map(str::to_string),
        default_steps: metadata.default_steps,
        default_width: metadata.default_width,
        default_height: metadata.default_height,
        loaded: true,
        supported: true,
        placeholders: vec![
            PROMPT.to_string(),
            INPUT_IMAGE.to_string(),
            FILENAME_PREFIX.to_string(),
            SEED.to_string(),
        ],
        warning: metadata.warning.map(str::to_string),
        reason: None,
    }
}

fn flux2_klein_9b_kv_experimental_support(default_workflow: &str) -> WorkflowSupport {
    WorkflowSupport {
        name: FLUX2_KLEIN_9B_KV_EXPERIMENTAL.to_string(),
        display_name: "FLUX 2 klein 9B-KV Experimental".to_string(),
        kind: "image_edit".to_string(),
        requires_input_image: true,
        experimental: true,
        default: default_workflow == FLUX2_KLEIN_9B_KV_EXPERIMENTAL,
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

/// Load a single explicitly-enabled workflow JSON by name.
pub fn load_template(dir: &Path, name: &str) -> anyhow::Result<Value> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("invalid workflow name: {name:?}");
    }
    let path = dir.join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
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

        let support = support_for_templates(&templates, "flux2_klein_edit");
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
        let registry = WorkflowRegistry {
            templates: HashMap::new(),
            support: HashMap::from([(
                FLUX2_KLEIN_9B_KV_EXPERIMENTAL.to_string(),
                flux2_klein_9b_kv_experimental_support("flux2_klein_edit"),
            )]),
        };
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
    fn load_template_loads_real_workflow() {
        // This test asserts the contract with project-zun: when the symlink
        // is present, the configured workflow loads cleanly. Skipped if the
        // symlink isn't set up in the dev env.
        let dir = std::path::Path::new("data/workflows");
        if !dir.exists() {
            eprintln!("data/workflows symlink missing; skipping real-template test");
            return;
        }
        let loaded = load_template(dir, "flux2_klein_edit").expect("load template");
        // Real workflow must still contain a PROMPT placeholder (the
        // contract). If this fails, either the symlink points somewhere
        // unexpected or project-zun's contract changed.
        let body = loaded.to_string();
        assert!(body.contains("PROMPT_PLACEHOLDER"));
        assert!(body.contains("INPUT_IMAGE_PLACEHOLDER"));
        assert!(body.contains("FILENAME_PREFIX_PLACEHOLDER"));
    }
}
