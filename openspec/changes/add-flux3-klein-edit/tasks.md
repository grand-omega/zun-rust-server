# Tasks

Privacy rule for every task (constitution VI): steps marked **[operator]** are run by the operator. Agents receive only HTTP status codes, job states, timings and VRAM numbers. They never receive job content, prompts, images or logs.

## 0. Preconditions

- [ ] 0.1 Confirm that the zun-flux-pipeline release containing `add-flux3-klein-edit` is tagged, and that its quality gate (pipeline task 3.7) recorded **go** in `doc/STACK.md`. Verify: the tag name and the gate line are cited in the PR description. On no-go, do every task except 2.1's `DEFAULT_WORKFLOW` change and the default asserts.

## 1. Embedded workflow assets

- [x] 1.1 Copy `workflows/flux3_klein_edit.json` from that pipeline tag. Verify: `cmp` against the pipeline's file reports no difference. Copied from zun-flux-pipeline branch `feat/flux3-klein-edit` (c586e6b) before the release tag exists; re-run `cmp` against the tag once it is cut.
- [x] 1.2 In `workflows/MANIFEST.yaml`, add only the `flux3_klein_edit` entry (identical to the pipeline's) and add `flux3_klein` to the family list comment. Leave the existing differences from the pipeline manifest as they are. Verify: `git diff workflows/MANIFEST.yaml` shows only those additions.

## 2. Workflow registry

- [ ] 2.1 In `src/workflow.rs`, set `ENABLED_WORKFLOWS = &["flux3_klein_edit", "flux2_klein_edit"]` and `DEFAULT_WORKFLOW = "flux3_klein_edit"`. Verify: `cargo build`.
- [ ] 2.2 Add a `"flux3_klein_edit"` arm to `workflow_metadata` (display name "FLUX 3 klein", kind `image_edit`, `requires_input_image: true`, not experimental, runtime `comfyui`, other fields `None`). Do not touch `humanize_workflow_name` or the dead `flux2_klein_9b_kv_edit` arm. Verify: the unit tests in 2.3 pass.
- [ ] 2.3 Unit tests in `src/workflow.rs`:
  - rename `flux2_klein_edit_remains_default` → `flux3_klein_edit_is_default` (FLUX 3 is default with "FLUX 3 klein"; FLUX 2 is supported with `default: false`);
  - add a `load_registry()` test: both real embedded templates are loaded, and exactly `flux3_klein_edit` is default;
  - extend `embedded_template_loads_and_carries_placeholders` to `flux3_klein_edit`;
  - add a case where `build_edit_workflow` on the real FLUX.3 template leaves no `_PLACEHOLDER` strings and sets `KSampler`'s seed to a JSON number.

  Verify: `cargo test --lib workflow`.
- [ ] 2.4 Integration tests:
  - change `tests/common/mod.rs:99` to use `workflow::DEFAULT_WORKFLOW`, and make the shared helper able to seed both workflows;
  - update `tests/health.rs`: `/capabilities` lists both workflows sorted by name, `flux3_klein_edit` has `default: true` and "FLUX 3 klein", and `flux2_klein_edit` has `default: false`;
  - add `tests/submit.rs` cases: a `prompt_text` submit with `workflow: "flux3_klein_edit"` stores that workflow, and a submit by `prompt_id` for a prompt stored with `flux2_klein_edit`, also carrying `workflow: "flux3_klein_edit"`, still stores `flux2_klein_edit`.

  Verify: `cargo test --locked`.

## 3. Timeout and runtime notes

- [ ] 3.1 **[operator]** On the RTX 4070 Ti Super with `warmup_on_start = false` and a 1 MP input, run each of these three times:
  - a warm FLUX.3 job;
  - a cold FLUX.3 job after a ComfyUI restart;
  - a cold FLUX.3 job after a restart with about 6 GB of VRAM held by another process, immediately after a FLUX 2 job (model swap);
  - a FLUX.3 → FLUX 2 swap.

  Also sample resident VRAM after warm-up. Verify: the operator reports every wall time and the resident VRAM, which are recorded in the PR description.
- [ ] 3.2 Set `DEFAULT_TIMEOUT_SECONDS` in `src/lib.rs` per design Decision 5, and rewrite its doc comment with the 3.1 numbers. If the new value exceeds the per-prompt accepted range, widen that range too. Verify: the new value is at least 1.5× the slowest 3.1 run, and `cargo test --locked` passes.
- [ ] 3.3 Update the FLUX2-specific comments in `src/worker.rs:13`, `src/comfy.rs:52` and the warm-up section of `config.example.toml` (:76-88), using the resident VRAM measured in 3.1. Ask the operator whether the "GPU also runs Jellyfin/Immich" note still describes the RTX 4070 Ti Super host, and correct it if not. Verify: `grep -n FLUX2 src config.example.toml` hits only lines that are still accurate.

## 4. Docs and quality gates

- [ ] 4.1 Update `docs/API_CONTRACT.md`:
  - the `/capabilities` example shows both workflows alphabetically, with FLUX.3 as default;
  - the :87 note becomes "`flux2_klein_edit` and `flux3_klein_edit` (default) are wired up";
  - the timeout note (about :109-112) carries the 3.1 numbers;
  - a note that saved prompts keep their stored workflow and must be recreated to change it;
  - all job-state examples use `done`.

  Update the `README.md` summary lines (FLUX.3 setup, measured job time) and add the rollback procedure from design Decision 7. Verify: every example payload is valid JSON and names only workflows in `ENABLED_WORKFLOWS`.
- [ ] 4.2 Run the full gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` and `cargo audit`. Verify: all pass. The pre-commit hook is not bypassed.
- [ ] 4.3 **[operator]** End-to-end against live ComfyUI with `warmup_on_start = true`:
  - warm-up completes;
  - `GET /api/v1/capabilities` shows FLUX.3 as default;
  - one `POST /api/v1/jobs` each with `flux3_klein_edit` and `flux2_klein_edit` reaches `done` and its result download returns 200.

  Verify: the operator reports the status codes and job states.

## Workflow follow-up

- Do not stage the pre-existing uncommitted change in `src/error.rs`. It is unrelated.
- Tell the zun-android-app change `add-flux3-klein-edit` the concrete server tag that ships this change.
- Archive with `openspec archive add-flux3-klein-edit` after review and release.
