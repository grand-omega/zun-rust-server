# Tasks

Privacy rule for every task (constitution VI): steps marked **[operator]** are run by the operator. Agents receive only HTTP status codes, job states, timings and VRAM numbers. They never receive job content, prompts, images or logs.

## 0. Preconditions

- [ ] 0.1 Confirm that the zun-flux-pipeline release containing `add-flux3-klein-edit` is tagged, and that its quality gate (pipeline task 3.7) recorded **go** in `doc/STACK.md`. Verify: the tag name and the gate line are cited in the PR description. On no-go, do every task except 2.1's `DEFAULT_WORKFLOW` change and the default asserts.

## 1. Embedded workflow assets

- [x] 1.1 Copy `workflows/flux3_klein_edit.json` from that pipeline tag. Verify: `cmp` against the pipeline's file reports no difference. Copied from zun-flux-pipeline branch `feat/flux3-klein-edit` (c586e6b) before the release tag exists; re-run `cmp` against the tag once it is cut.
- [x] 1.2 In `workflows/MANIFEST.yaml`, add only the `flux3_klein_edit` entry (identical to the pipeline's) and add `flux3_klein` to the family list comment. Leave the existing differences from the pipeline manifest as they are. Verify: `git diff workflows/MANIFEST.yaml` shows only those additions.

## 2. Workflow registry

- [x] 2.1 In `src/workflow.rs`, set `ENABLED_WORKFLOWS = &["flux3_klein_edit", "flux2_klein_edit"]` and `DEFAULT_WORKFLOW = "flux3_klein_edit"`. Verify: `cargo build`.
- [x] 2.2 Add a `"flux3_klein_edit"` arm to `workflow_metadata` (display name "FLUX 3 klein", kind `image_edit`, `requires_input_image: true`, not experimental, runtime `comfyui`, other fields `None`). Do not touch `humanize_workflow_name` or the dead `flux2_klein_9b_kv_edit` arm. Verify: the unit tests in 2.3 pass.
- [x] 2.3 Unit tests in `src/workflow.rs`:
  - rename `flux2_klein_edit_remains_default` → `flux3_klein_edit_is_default` (FLUX 3 is default with "FLUX 3 klein"; FLUX 2 is supported with `default: false`);
  - add a `load_registry()` test: both real embedded templates are loaded, and exactly `flux3_klein_edit` is default;
  - extend `embedded_template_loads_and_carries_placeholders` to `flux3_klein_edit`;
  - add a case where `build_edit_workflow` on the real FLUX.3 template leaves no `_PLACEHOLDER` strings and sets `KSampler`'s seed to a JSON number.

  Verify: `cargo test --lib workflow`.
- [x] 2.4 Integration tests:
  - change `tests/common/mod.rs:99` to use `workflow::DEFAULT_WORKFLOW`, and make the shared helper able to seed both workflows;
  - update `tests/health.rs`: `/capabilities` lists both workflows sorted by name, `flux3_klein_edit` has `default: true` and "FLUX 3 klein", and `flux2_klein_edit` has `default: false`;
  - add `tests/submit.rs` cases: a `prompt_text` submit with `workflow: "flux3_klein_edit"` stores that workflow, and a submit by `prompt_id` for a prompt stored with `flux2_klein_edit`, also carrying `workflow: "flux3_klein_edit"`, still stores `flux2_klein_edit`.

  Verify: `cargo test --locked`.

## 3. Timeout and runtime notes

- [x] 3.1 **[operator]** On the RTX 4070 Ti Super with `warmup_on_start = false` and a 1 MP input, run each of these three times:
  - a warm FLUX.3 job;
  - a cold FLUX.3 job after a ComfyUI restart;
  - a cold FLUX.3 job after a restart with about 6 GB of VRAM held by another process, immediately after a FLUX 2 job (model swap);
  - a FLUX.3 → FLUX 2 swap.

  Also sample resident VRAM after warm-up. Verify: the operator reports every wall time and the resident VRAM, which are recorded in the PR description.
  - Done 2026-10-27 on gpu-dev-4004 (RTX 4070 Ti Super), staging server (this branch, release build) → staging ComfyUI v0.42.4 on :8189, `warmup_on_start = false`, synthetic 1 MP PNGs (a new image and a new saved prompt per job, so no ComfyUI cache hits). Wall time is from `POST /api/v1/jobs` until `GET /jobs/{id}?wait=25` returned `done`. All 18 jobs reached `done`, and every result download returned 200.
    - warm FLUX.3: 7.6, 7.9, 7.7 s;
    - cold FLUX.3, first job after a ComfyUI restart: 18.7, 18.4, 18.4 s;
    - contended cold FLUX.3: 16.4, 16.5, 16.7 s. A 6 GiB CUDA allocation was held by another process (whole-GPU memory.used 6398 MiB), then ComfyUI restarted, ran a FLUX 2 job (14.6, 15.4, 15.5 s), and the timed FLUX.3 job followed immediately. That order gives both "after a restart" and "FLUX 2 resident beforehand";
    - FLUX.3 → FLUX 2 swap: 10.3, 10.1, 9.6 s (FLUX 2 → FLUX.3, for reference: 15.5, 15.7 s).
  - Resident VRAM: 9964 MiB whole-GPU memory.used, idle, after warm FLUX.3 jobs and again after the server's own warm-up (task 4.3). Peak sampled VRAM: 13986 MiB cold, 13114 MiB warm, 15785 MiB contended (6398 MiB of that held by the other process).
  - The OS page cache held the weights in every run. No run measured a cold disk read.
  - Build provenance of that first matrix: a release build of the unmodified starting commit `60c79e1`, finished 03:37:46 (no tracked file changed before 04:43:58). That file was later overwritten and its hash was not retained. An exported (`git archive`) build of `60c79e1` gives sha256 `5a15490f…`, the same as a same-path rebuild, but that is a reconstruction, not the measured file.
  - **Acceptance rerun with an exact build, 2026-10-27 05:31–05:37 UTC** (window `w3b`). Same matrix and harness, fresh config, data dir and DB.
    - Binary: built from a `git archive` export of commit `6b51a1e` (tree `db326ad`) in a separate directory, rustc/cargo 1.99.1. sha256 `dabeed0932ef953714e39a260ca9674a380da678d0b206b0c6963a2f503c2a96`, checked at the start and end of the window.
    - Gates in that export: fmt and clippy `-D warnings` exit 0; `cargo test --locked` 136 passed, 0 failed, 1 ignored doc test; `cargo audit` exit 0. Staging pipeline was at `c1400d4`, ComfyUI at `3f438ca`.
    - All 18 jobs: submit 202, `done`, result 200.
    - Results: warm FLUX.3 7.7, 7.9, 7.7 s; cold FLUX.3 after a restart 21.3, 19.6, 19.1 s; contended cold FLUX.3 16.3, 16.6, 17.4 s (preceding FLUX 2 jobs 14.5, 15.1, 15.2 s); FLUX.3 → FLUX 2 swap 10.4, 10.1, 9.3 s (FLUX 2 → FLUX.3 15.9, 15.5 s).
    - Resident VRAM 9964 MiB. Peaks: 13986 MiB cold, 13114 MiB warm, 15753–15785 MiB contended (6398 MiB of that held by the other process).
    - Records, binary and gate transcripts: `~/grand-omega-flux3/.artifacts/2026-10-27-flux3-validation/` on gpu-dev-4004.
- [x] 3.2 Set `DEFAULT_TIMEOUT_SECONDS` in `src/lib.rs` per design Decision 5, and rewrite its doc comment with the 3.1 numbers. If the new value exceeds the per-prompt accepted range, widen that range too. Verify: the new value is at least 1.5× the slowest 3.1 run, and `cargo test --locked` passes.
  - Plan before 3.1 ran (now carried out, see the next bullet): measure, then apply `ceil(1.5 × slowest / 30) × 30`; widen `MAX_TIMEOUT_SECONDS` only above 1800. Update the `src/lib.rs` comment and the timeout notes in `docs/API_CONTRACT.md`, `README.md` and `config.example.toml`, then run fmt, clippy, `cargo test --locked` and `cargo audit`. No test hardcodes 120; tests read the constant. `REQUEST_TIMEOUT` (`src/lib.rs`, HTTP per-request) is unrelated.
  - Done 2026-10-27. Across both 3.1 matrices the slowest run was 21.3 s (acceptance rerun), so the rule's floor is `ceil(1.5 × 21.3 / 30) × 30` = 60 s. The first matrix alone (slowest 18.7 s) gave 30 s. `DEFAULT_TIMEOUT_SECONDS` stays at 120 s. That still satisfies design Decision 5 ("at least" the floor, a multiple of 30), and one default also covers FLUX 2. FLUX 2's first job after a restart has taken ~105 s under heavy VRAM contention, which is why 60 s was raised to 120 s; dropping to 30 s would reintroduce that failure. `MAX_TIMEOUT_SECONDS` is unchanged. The comment at `src/lib.rs` carries the first matrix's numbers and its 30 s floor. It is deliberately left unedited after the rerun, so that `src/` stays identical to the measured exact build `6b51a1e`. `docs/API_CONTRACT.md`, `README.md` and `config.example.toml` give the range across both matrices. `API_CONTRACT.md:25` (`default 120`) and `config.example.toml:81-89` stay accurate. fmt, clippy `-D warnings`, `cargo test --locked` (136 passed, 1 ignored) and `cargo audit` (exit 0; the same 2 default-allowed warnings) pass. The spec scenario "contended cold start with model swap" also ran on the default timeout (`prompt_text` jobs, no per-prompt timeout): FLUX 2 15.7 s, then FLUX.3 17.0 s, both `done`.
- [x] 3.3 Update the FLUX2-specific comments in `src/worker.rs:13`, `src/comfy.rs:52` and the warm-up section of `config.example.toml` (:76-88), using the resident VRAM measured in 3.1. Ask the operator whether the "GPU also runs Jellyfin/Immich" note still describes the RTX 4070 Ti Super host, and correct it if not. Verify: `grep -n FLUX2 src config.example.toml` hits only lines that are still accurate. The measured FLUX.3 resident VRAM and timings were filled in on 2026-10-27. Jellyfin/Immich: the RTX 4070 Ti Super host gpu-dev-4004 runs neither (no systemd unit, no container; checked 2026-10-27), and `config.example.toml` now says so.

## 4. Docs and quality gates

- [x] 4.1 Update `docs/API_CONTRACT.md`:
  - the `/capabilities` example shows both workflows alphabetically, with FLUX.3 as default;
  - the :87 note becomes "`flux2_klein_edit` and `flux3_klein_edit` (default) are wired up";
  - the timeout note (about :109-112) carries the 3.1 numbers;
  - a note that saved prompts keep their stored workflow and must be recreated to change it;
  - all job-state examples use `done`.

  Update the `README.md` summary lines (FLUX.3 setup, measured job time) and add the rollback procedure from design Decision 7. Verify: every example payload is valid JSON and names only workflows in `ENABLED_WORKFLOWS`. The 3.1 timeout numbers and the measured FLUX.3 job time were filled in on 2026-10-27.
- [x] 4.2 Run the full gate: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` and `cargo audit`. Verify: all pass. The pre-commit hook is not bypassed. Status 2026-10-26: `cargo audit` initially failed on 10 advisories in dependencies inherited from `origin/dev`; remediated with patch-level `Cargo.lock` bumps only (ctutils 0.4.4, quinn-proto 0.11.18, rustls 0.23.46, rustls-webpki 0.103.16, webpki-root-certs 1.0.10). After that, `cargo audit` exits 0 (0 vulnerabilities; 2 default-allowed warnings, paste unmaintained and tokio unsound, not suppressed), and `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --locked` (137 passed) pass.
- [x] 4.3 **[operator]** End-to-end against live ComfyUI with `warmup_on_start = true`:
  - warm-up completes;
  - `GET /api/v1/capabilities` shows FLUX.3 as default;
  - one `POST /api/v1/jobs` each with `flux3_klein_edit` and `flux2_klein_edit` reaches `done` and its result download returns 200.

  Verify: the operator reports the status codes and job states.
  - Done 2026-10-27 on gpu-dev-4004 against the staging server (this branch) and staging ComfyUI v0.42.4 (:8189) with `warmup_on_start = true`. Warm-up completed (`warmup.done` logged once, no failure) 20.7 s after server start, and 9964 MiB was in use after it. `GET /health` returned 200. `GET /capabilities` returned 200 and listed `flux2_klein_edit` (default false) and `flux3_klein_edit` ("FLUX 3 klein", default true). `POST /jobs` returned 202 for each workflow, and both jobs reached `done` (`flux3_klein_edit` 7.9 s, `flux2_klein_edit` 10.5 s). Each `GET /jobs/{id}/result` returned 200.
  - Build provenance: release binary sha256 `dabeed0932ef953714e39a260ca9674a380da678d0b206b0c6963a2f503c2a96`, built 04:46:07 in the working tree from the source later committed as `1bd1dab`. Independently, a `git archive` export of `6b51a1e` (same `src/`, `Cargo.*` and `workflows/`), built in a separate directory, produced the same sha256. The gates passed in that export (see 3.1). Gates for that tree passed: fmt, clippy `-D warnings`, `cargo test --locked` (136 passed, 1 ignored doc test) and `cargo audit` exit 0. The binary and the full gate transcript are retained in the same artifacts directory.

## Workflow follow-up

- Do not stage the pre-existing uncommitted change in `src/error.rs`. It is unrelated.
- Tell the zun-android-app change `add-flux3-klein-edit` the concrete server tag that ships this change.
- Archive with `openspec archive add-flux3-klein-edit` after review and release.
