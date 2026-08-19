# Implementation Plan: CI/CD Evaluation & Security Hardening

**Branch**: `001-ci-cd-security-hardening` | **Date**: 2026-07-05 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/001-ci-cd-security-hardening/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command. See `.specify/templates/plan-template.md` for the execution workflow.

## Summary

Produce a prioritized security audit and a CI/CD gap evaluation for
zun-rust-server (a single-user Axum + SQLite service, ~4.5K lines across a
flat `src/*.rs` module layout), then apply regression-tested fixes for the
findings that are within this codebase's control and consistent with the
project constitution's single-user, no-added-configurability stance. The
technical approach is: run existing/standard Rust security tooling
(`cargo audit`, `cargo clippy`) plus manual review of the auth, path, and
input-handling code paths already centralized in `src/auth.rs` and
`src/paths.rs`; write findings as two plain-markdown reports; add the one
or two CI gates that would have caught real findings; fix and test the
rest in-place.

## Technical Context

**Language/Version**: Rust stable, edition 2024 (currently `rustc 1.96.1`)

**Primary Dependencies**: `axum` 0.8 (+ multipart), `tokio` (full), `sqlx`
0.9 (SQLite, WAL), `reqwest` 0.13 (rustls, no OpenSSL), `tower-http`
(trace, sensitive-headers, timeout), `tracing` + `tracing-subscriber`

**Storage**: SQLite via `sqlx` (`jobs.db` in `data_dir`); flat files under
`data_dir/{cache/inputs,outputs,thumbs,previews,backups}`

**Testing**: `cargo test` with integration tests in `tests/*.rs`
(`auth.rs`, `gallery.rs`, `health.rs`, `images.rs`, `prompts.rs`,
`submit.rs`, `worker.rs`), `wiremock` for mocking the ComfyUI/pipeline HTTP
boundary, `tempfile` for isolated `data_dir`s, `http-body-util` for
request/response bodies

**Target Platform**: Linux server; DEV = phone on LAN behind no proxy,
PROD = behind a same-host reverse proxy (Rocky/RHEL) that terminates TLS

**Project Type**: Single-crate web service (one binary, `zun-rust-server`)

**Performance Goals**: None beyond current behavior — this is a
single-user, single-GPU-job-at-a-time system (see constitution Principle
I); the goal is correctness/robustness under malformed or hostile input,
not throughput.

**Constraints**: Must keep passing `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, `cargo test` (constitution Principle III);
must not add multi-tenant defenses, per-IP limits, or session state beyond
the existing bearer token (Principle IV); must not add new config
knobs/env vars/feature flags to remediate findings unless a finding
specifically requires it and that's surfaced explicitly (Principle V);
must not read/log real job payload or prompt content while auditing
(Principle VI).

**Scale/Scope**: ~4,500 lines across 17 `src/*.rs` modules, 7 integration
test files, one GitHub Actions workflow (`rust.yml`) with one job.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Check | Status |
|---|---|---|
| I. Single-User Simplicity | Findings/recommendations must not introduce multi-tenant scaffolding (rate limiting, tenant isolation, health probing for external consumers) | PASS — spec FR-006/Edge Cases explicitly scope this out |
| II. Surgical Changes | Story 3 fixes must be scoped to the specific finding, not drive-by refactors | PASS — plan restricts fixes to file/line named in each finding |
| III. Quality Gates Are Non-Negotiable | Every Story 3 fix needs a regression test; fmt/clippy/test must pass | PASS — spec FR-005/FR-007 |
| IV. Explicit Security Boundary | Findings must not recommend defense-in-depth the constitution rules out (in-app rate limiting, IP allowlisting) unless arguing the boundary itself is broken | PASS — spec FR-006 |
| V. Compile-Time Config Over Runtime Flexibility | No new config knobs/feature flags to fix findings unless explicitly required and surfaced | PASS — spec Assumptions |
| VI. No Inspection of Sensitive Job/Log Content | Audit must evaluate file/path handling via code review + metadata, never by reading real job payload/log content | PASS — spec FR-008 |

No violations. Complexity Tracking table is not needed.

## Project Structure

### Documentation (this feature)

```text
specs/001-ci-cd-security-hardening/
├── plan.md                  # This file (/speckit-plan command output)
├── research.md              # Phase 0 output (/speckit-plan command)
├── data-model.md            # Phase 1 output (/speckit-plan command)
├── quickstart.md            # Phase 1 output (/speckit-plan command)
├── security-audit.md        # Phase 1 output — Story 1 deliverable (the audit report itself)
├── ci-cd-evaluation.md      # Phase 1 output — Story 2 deliverable (the CI evaluation itself)
├── checklists/requirements.md
└── tasks.md                 # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

There is no `contracts/` directory: this feature adds no new external
interface (no new HTTP endpoint, no new CLI surface). Its "contracts" are
the two report documents above, whose required structure is captured in
`data-model.md`.

### Source Code (repository root)

```text
src/
├── main.rs             # entrypoint, CLI arg parsing
├── lib.rs              # module wiring
├── auth.rs             # bearer-token check — primary Story 1 audit target
├── config.rs           # config.toml parsing — token/bind/paths validation
├── handlers.rs         # axum route handlers — primary Story 1 audit target (input validation)
├── paths.rs            # data_dir path assembly — traversal guard already centralized here
├── inputs.rs           # uploaded-image handling — primary Story 1 audit target
├── images.rs           # image decode/encode (via `image` crate)
├── comfy.rs            # outbound HTTP client to the ComfyUI/pipeline backend
├── db.rs               # sqlx/SQLite access
├── worker.rs           # job queue processing
├── backup.rs / purge.rs
├── derived_images.rs / custom_prompts.rs / workflow.rs / hash.rs / error.rs / state.rs / logging.rs
tests/
├── auth.rs, health.rs, images.rs, prompts.rs, submit.rs, gallery.rs, worker.rs
.github/workflows/
└── rust.yml            # existing CI: forbidden-path check, fmt, clippy, test — Story 2 audit target
.githooks/
└── pre-commit, pre-push
scripts/
└── check-forbidden-paths.sh
```

**Structure Decision**: Single existing crate, no new top-level
directories. Story 1/2 deliverables are new markdown files under this
feature's `specs/` directory (not `docs/`, to keep this audit/evaluation
attached to the feature that produced it — the operator can promote
durable findings into `README.md`/`docs/` afterward if desired, as a
separate, explicit decision). Story 3 fixes land as normal edits inside
the existing `src/*.rs` files and matching new/extended tests inside the
existing `tests/*.rs` files — no new module layout.

## Complexity Tracking

*No constitution violations — this table is intentionally empty.*
