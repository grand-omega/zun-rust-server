# Spec Delta

## Purpose

Defines which image-edit workflows the server accepts for jobs and saved prompts, which one it advertises as the default to clients, how `/capabilities` describes them, and the default job timeout that covers them.

## ADDED Requirements

### Requirement: FLUX.3 klein edit is the default workflow
`GET /api/v1/capabilities` SHALL list `flux3_klein_edit` as a supported image-edit workflow with display name "FLUX 3 klein", `requires_input_image: true`, `experimental: false` and `default: true`. Exactly one listed workflow SHALL have `default: true`.

#### Scenario: Capabilities advertise FLUX.3 as default
- **WHEN** a client calls `GET /api/v1/capabilities` on a production build
- **THEN** the `workflows` array contains `flux3_klein_edit` with `display_name` "FLUX 3 klein" and `default: true`, and no other entry has `default: true`

### Requirement: FLUX 2 klein edit remains supported
`flux2_klein_edit` SHALL stay listed in `/capabilities` as supported, with `default: false`. Jobs and saved prompts that name it SHALL keep being accepted and processed.

#### Scenario: Old client submits FLUX 2 job
- **WHEN** a client submits `POST /api/v1/jobs` with `prompt_text` and `workflow: "flux2_klein_edit"`
- **THEN** the job is accepted and runs the FLUX 2 klein template

#### Scenario: Existing saved prompt still runs
- **WHEN** a client submits a job with the `prompt_id` of a saved prompt whose stored workflow is `flux2_klein_edit`
- **THEN** the job is accepted and runs with `flux2_klein_edit`, whatever `workflow` value the request also carries

### Requirement: FLUX.3 jobs use the FLUX.3 template
A job submitted with workflow `flux3_klein_edit` SHALL be sent to ComfyUI as the embedded `flux3_klein_edit` template. The prompt, input image, per-job filename prefix (`zun_<job_id>`) and seed SHALL be substituted exactly as for other edit workflows.

#### Scenario: FLUX.3 job completes
- **WHEN** a client submits an image and prompt text with `workflow: "flux3_klein_edit"` and ComfyUI is running the FLUX.3-capable pipeline
- **THEN** the job reaches status `done`, and its result image downloads through the existing job endpoints

#### Scenario: Saved prompt bound to FLUX.3
- **WHEN** a client creates a saved prompt with `workflow: "flux3_klein_edit"` and submits a job by that `prompt_id`
- **THEN** the job is accepted and runs the FLUX.3 template

### Requirement: Default job timeout covers a FLUX.3 cold start
The default per-job timeout SHALL be at least 1.5 times the slowest measured first job after a ComfyUI restart, under these conditions:
- the RTX 4070 Ti Super;
- warm-up disabled;
- a 1 MP input;
- about 6 GB of VRAM held by another process;
- FLUX 2 resident beforehand, so FLUX.3 has to swap in.

#### Scenario: Contended cold start with model swap
- **WHEN** a FLUX 2 job has just run, then ComfyUI restarts with about 6 GB of VRAM held elsewhere, and a `flux3_klein_edit` job with no per-prompt timeout is submitted
- **THEN** the job reaches `done` before the default timeout expires
