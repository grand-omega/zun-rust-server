# Proposal

## Why

zun-flux-pipeline is adding a FLUX.3 Image Klein edit workflow (change `add-flux3-klein-edit` there), the successor to FLUX 2 klein, and is gating its promotion on an operator quality check. The server only exposes workflows compiled into it, so FLUX.3 needs a server change before any client can use it.

## What Changes

- **Templates:** embed the pipeline's `flux3_klein_edit.json` byte-for-byte, and add its entry to `workflows/MANIFEST.yaml`. Existing manifest differences are left as they are.
- **Default workflow:** enable `flux3_klein_edit` with display name "FLUX 3 klein" and make it the **default** in `GET /api/v1/capabilities`. This flip proceeds only after the pipeline's quality gate records a "go".
- **FLUX 2 kept:** `flux2_klein_edit` stays enabled and submittable, just no longer the default. Old clients, saved prompts and job history keep working.
- **Timeout:** re-measure the cold-start time on FLUX.3, set the default job timeout from that measurement, and update every place that documents it.
- **Docs:** update `README.md` and `docs/API_CONTRACT.md`, including a coordinated rollback procedure.
- **Not breaking:** no endpoint, field, status value or error code changes. Only which entry has `default: true` in `/capabilities` moves.

Non-goals:
- No DB migration.
- Saved prompts are not rewritten to the new workflow.
- No new runtime config knob.
- No humanize rule for hypothetical future flux3 workflows.

Reported, not fixed (constitution II): the dead `flux2_klein_9b_kv_edit` arm in `workflow_metadata` (`src/workflow.rs:241`) is unreachable. It is left as is.

## Capabilities

### New Capabilities
- `edit-workflow-catalog`: which image-edit workflows the server accepts, which one it advertises as default through `/capabilities`, how they are described, and the default job timeout that covers them.

### Modified Capabilities
<!-- None: the repo has no specs under openspec/specs/ yet. -->

## Impact

- **Code:**
  - `src/workflow.rs`: `ENABLED_WORKFLOWS`, `DEFAULT_WORKFLOW`, a `workflow_metadata` arm, unit tests.
  - `src/lib.rs`: `DEFAULT_TIMEOUT_SECONDS` and its doc comment.
  - Comments in `src/worker.rs` and `src/comfy.rs`, and the warm-up section of `config.example.toml` (:76-88).
- **Embedded assets:** `workflows/flux3_klein_edit.json` and one new `MANIFEST.yaml` entry.
- **Tests:**
  - `tests/common/mod.rs:99` hardcodes the default today; it changes to follow `DEFAULT_WORKFLOW`.
  - `tests/health.rs`.
  - New unit and integration cases.
- **API:** `/capabilities` lists two workflows, sorted by name. `default: true` moves to `flux3_klein_edit`. The Android app follows in its own change.
- **Ops:**
  - Warm-up (`warmup_on_start`) now loads FLUX.3.
  - Production inference runs on the RTX 4070 Ti Super (16 GB).
  - Requires the pipeline release that contains `flux3_klein_edit` (ComfyUI v0.42.4).
- **Ordering:** the pipeline release and its quality gate come first. This change lands before the Android change.
