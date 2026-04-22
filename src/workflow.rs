//! ComfyUI workflow template loading and placeholder substitution.
//!
//! Templates are opaque JSON blobs owned by the sibling project-zun repo
//! (see `doc/WORKFLOWS.md` there for the full placeholder contract). This
//! module loads them and performs **whole-string** substitution on known
//! placeholder tokens. Substring matches are intentional non-matches:
//! `PROMPT_PLACEHOLDER` must occupy an entire JSON string value.
use std::{collections::HashMap, path::Path};

use serde_json::Value;

// --- Placeholder tokens (mirrors project-zun/doc/WORKFLOWS.md). -----------

pub const PROMPT: &str = "PROMPT_PLACEHOLDER";
pub const INPUT_IMAGE: &str = "INPUT_IMAGE_PLACEHOLDER";
pub const FILENAME_PREFIX: &str = "FILENAME_PREFIX_PLACEHOLDER";
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
/// Supplies the three placeholders every edit workflow needs. Extra
/// workflow-specific placeholders (mask prompt, reference image, lora) are
/// the caller's responsibility via `patch_placeholders` directly.
pub fn build_edit_workflow(
    template: &Value,
    prompt_text: &str,
    input_image_name: &str,
    job_id: &str,
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
    out
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
        // Fixture mirrors the shape of flux2_klein_edit (CLIPTextEncode on
        // some node with "text": PROMPT, LoadImage with "image": INPUT,
        // SaveImage with "filename_prefix": FILENAME_PREFIX).
        let template = json!({
            "4": { "inputs": { "image": "INPUT_IMAGE_PLACEHOLDER" } },
            "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } },
            "19": { "inputs": { "filename_prefix": "FILENAME_PREFIX_PLACEHOLDER" } }
        });
        let built = build_edit_workflow(&template, "anime", "in.jpg", "abc-123");
        assert_eq!(built["9"]["inputs"]["text"], "anime");
        assert_eq!(built["4"]["inputs"]["image"], "in.jpg");
        assert_eq!(built["19"]["inputs"]["filename_prefix"], "zun_abc-123");
    }

    #[test]
    fn build_edit_workflow_does_not_touch_original() {
        let template = json!({ "9": { "inputs": { "text": "PROMPT_PLACEHOLDER" } } });
        let _ = build_edit_workflow(&template, "x", "y.jpg", "j");
        assert_eq!(template["9"]["inputs"]["text"], "PROMPT_PLACEHOLDER");
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
