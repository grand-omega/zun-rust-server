# Design

## Context

- **Compile-time workflows** (constitution V): `include_dir!` embeds `workflows/`, `ENABLED_WORKFLOWS` gates what is exposed, and `DEFAULT_WORKFLOW` picks the `default: true` entry.
- **`/capabilities` ordering:** `support_list()` (`workflow.rs:102-106`) sorts by name, so the response lists flux2 before flux3 whatever the order of `ENABLED_WORKFLOWS`.
- **Substitution:** `build_edit_workflow` substitutes placeholders by exact string match on field values, so node classes and ids don't matter. The pipeline guarantees that `flux3_klein_edit` carries the same four literal placeholders.
- **Default is advisory:** `submit_job` never falls back to `DEFAULT_WORKFLOW`.
  - `prompt_text` submissions must name a workflow.
  - `prompt_id` submissions use the prompt's stored workflow and ignore any `workflow` field in the request.
- **Saved prompts are pinned:** app-side prompt edits change only label and text, not workflow (`RealJobRepository.kt:120`). A prompt's workflow is fixed at creation.
- **Test default is hardcoded:** `tests/common/mod.rs:99` builds the integration-test registry with `"flux2_klein_edit"` as its default.
- **Manifest drift:** the server's `MANIFEST.yaml` has already drifted from the pipeline's (old GDINO/SAM notes, no `status: paused`). Nothing in `src/` reads it.
- **Current timing:** the FLUX 2 numbers in `lib.rs:74-79` are about 3 s warm, about 20 s cold, and about 105 s cold under contention, while staging about 4 GB. FLUX.3 stages about 10.7 GB and runs 16 steps instead of 4.

## Goals / Non-Goals

**Goals:**
- FLUX.3 available and advertised as default, with FLUX 2 runnable side by side.
- A rollback path that doesn't strand queued work silently.

**Non-Goals:**
- No migration of `custom_prompts.workflow`.
- No per-workflow timeout table.
- No reconciling of the existing manifest drift.
- No changes to the experimental or `9b_kv` paths.

## Decisions

1. **`ENABLED_WORKFLOWS = ["flux3_klein_edit", "flux2_klein_edit"]` and `DEFAULT_WORKFLOW = "flux3_klein_edit"`.**
   - The list order is irrelevant to the API (it is sorted by name).
   - Alternative: remove FLUX 2. Rejected; it strands saved prompts and old app builds.

2. **Add an explicit `workflow_metadata` arm** for `flux3_klein_edit` that mirrors the `flux2_klein_edit` arm.
   - No `humanize_workflow_name` change: no workflow needs it (constitution II).

3. **Make the test default follow production.**
   - `tests/common/mod.rs` uses `workflow::DEFAULT_WORKFLOW` instead of the literal string.
   - The health test seeds both workflows.
   - Add a `load_registry()` unit test asserting that both real embedded templates are enabled and that exactly `flux3_klein_edit` is default. This proves the production wiring, not just a hand-built registry.
   - Rename `flux2_klein_edit_remains_default` → `flux3_klein_edit_is_default`. This is a test our own change invalidates, so it complies with constitution II.

4. **Manifest: add only the new entry and the family-list update.** Existing differences stay as they are (surgical). The workflow JSON itself is byte-identical to the pipeline copy.

5. **The timeout is set by measurement.** Measure under the spec's stated conditions, three runs each, and set `DEFAULT_TIMEOUT_SECONDS` to at least 1.5× the slowest run, rounded up to a multiple of 30 s.
   - It must stay within the existing per-prompt accepted range, or that range is updated too.
   - Rewrite the `lib.rs` doc comment with the FLUX.3 numbers, and update `docs/API_CONTRACT.md`'s timeout note (it still says "stages ~4 GB").

6. **Warm-up keeps using `DEFAULT_WORKFLOW`** (no code change). The `config.example.toml` warm-up comment gets measured resident VRAM, not file sizes.

7. **Rollback is coordinated.** Reverting this change removes FLUX.3 support, which:
   - fails saved prompts bound to `flux3_klein_edit` at submit (`handlers.rs:245`) and on edit (`custom_prompts.rs:75`);
   - fails queued `flux3_klein_edit` jobs at `worker.rs:390`.

   Preferred rollback is a **forward fix**: a release that sets `DEFAULT_WORKFLOW` back to `flux2_klein_edit` and keeps FLUX.3 enabled, as long as the pipeline runtime still supports it.

   Full revert procedure:
   1. Stop the app from submitting.
   2. Let the queue drain, or cancel the remaining FLUX.3 jobs.
   3. Revert the Android default.
   4. Revert the server.
   5. Only then revert the pipeline pin.

   Saved prompts bound to FLUX.3 must be deleted and recreated, because the app can't change a prompt's workflow.

## Risks / Trade-offs

- **The pipeline and server copies of `flux3_klein_edit.json` drift apart.**
  → Mitigation: a `cmp` task, plus the embedded-template unit test.
- **Saved prompts silently stay on FLUX 2** while the UI shows FLUX 3 as default.
  → Mitigation: documented in `API_CONTRACT.md`. To move a prompt to FLUX.3, the operator recreates it.
- **Model swapping between FLUX 2 and FLUX 3 jobs** thrashes VRAM. Single-threaded jobs bound the impact, and the timeout measurement includes a swap.
- **The cold start may well exceed 180 s**, given 2.7× the bytes and 4× the steps.
  → Mitigation: Decision 5 takes no anchor and lets the measurement decide.

## Migration Plan

1. Deploy the pipeline release, then run its quality gate. A no-go stops here: ship FLUX.3 enabled but keep `DEFAULT_WORKFLOW = "flux2_klein_edit"`.
2. Deploy the server.
3. Update the Android app.

Rollback: see Decision 7.
