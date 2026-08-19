---

description: "Task list for CI/CD Evaluation & Security Hardening"
---

# Tasks: CI/CD Evaluation & Security Hardening

**Input**: Design documents from `/specs/001-ci-cd-security-hardening/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, quickstart.md

**Tests**: Story 3 fixes require a regression test per fix (constitution
Principle III / spec FR-005); Stories 1 and 2 are documentation
deliverables and have no test tasks of their own.

**Organization**: Tasks are grouped by user story to enable independent
implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Single Rust crate at repo root: `src/*.rs`, `tests/*.rs`,
`.github/workflows/rust.yml`. Feature deliverables live under
`specs/001-ci-cd-security-hardening/`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Prepare the two report files that Stories 1 and 2 populate

- [X] T001 Create `specs/001-ci-cd-security-hardening/security-audit.md` with a Markdown table header matching the `Finding` fields in `specs/001-ci-cd-security-hardening/data-model.md` (id, severity, area, location, description, rationale, recommended_fix, status)
- [X] T002 [P] Create `specs/001-ci-cd-security-hardening/ci-cd-evaluation.md` with a Markdown table header matching the `CI Gate` fields in `specs/001-ci-cd-security-hardening/data-model.md` (name, status, catches, does_not_catch, cost, decision)
- [X] T003 [P] Confirm `cargo-audit` is available locally (`cargo install cargo-audit` if `cargo audit --version` fails) so Story 1's dependency-scan task can run

**Checkpoint**: Both report skeletons exist; tooling is ready

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Establish a clean baseline before any fix work, so a test
failure introduced by Story 3 is never confused with pre-existing state

**⚠️ CRITICAL**: Must complete before Story 3 (Phase 5) begins

- [X] T004 Run `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test` from repo root and confirm it passes clean on the current `main`, recording this as the baseline before any Story 3 change

**Checkpoint**: Baseline confirmed green — Story 3 fixes can be judged against it

---

## Phase 3: User Story 1 - Security Audit Report (Priority: P1) 🎯 MVP

**Goal**: A written, reviewable report (`security-audit.md`) enumerating
security findings across auth/token handling, input validation,
dependency vulnerabilities, file/path handling, and the outbound
ComfyUI/pipeline HTTP boundary — each finding fully specified per
`data-model.md`.

**Independent Test**: Read `security-audit.md` end to end; every row has
all `Finding` fields populated and no code has changed. Satisfies
spec SC-001.

### Implementation for User Story 1

- [X] T005 [US1] Audit token/auth handling in `src/auth.rs` and `src/config.rs` (token loading, comparison, minimum-length/format validation, `Authorization` header parsing); append `SEC-*` rows to `specs/001-ci-cd-security-hardening/security-audit.md`
- [X] T006 [US1] Audit request input validation in `src/handlers.rs` and `src/inputs.rs` (multipart body size limits, content-type/extension checks, JSON body limits, malformed-input handling); append `SEC-*` rows to `specs/001-ci-cd-security-hardening/security-audit.md`
- [X] T007 [US1] Run `cargo audit` from repo root against `Cargo.lock`; append a `SEC-*` row per advisory/warning found (cross-check against `specs/001-ci-cd-security-hardening/research.md` R1's 2026-07-05 baseline — paste unmaintained, num-bigint yanked, 0 CVEs — and re-verify since the advisory DB changes over time) to `specs/001-ci-cd-security-hardening/security-audit.md`
- [X] T008 [US1] Audit file/path handling in `src/paths.rs`, `src/db.rs`, `src/backup.rs`, and `src/purge.rs` (traversal guards, atomic-write behavior, backup/purge race conditions); append `SEC-*` rows to `specs/001-ci-cd-security-hardening/security-audit.md`
- [X] T009 [US1] Audit the outbound HTTP boundary in `src/comfy.rs` (TLS verification via rustls, timeout/retry behavior, response size handling, trust level of the configured `comfy_url`); append `SEC-*` rows to `specs/001-ci-cd-security-hardening/security-audit.md`
- [X] T010 [US1] Review all rows in `specs/001-ci-cd-security-hardening/security-audit.md` against the `Finding` validation rules in `data-model.md` (every field populated; no finding recommends multi-tenant defense-in-depth without explicit justification per constitution Principle IV); fix any incomplete rows

**Checkpoint**: `security-audit.md` is complete and independently reviewable — this alone is a shippable increment (spec SC-001)

---

## Phase 4: User Story 2 - CI/CD Pipeline Evaluation (Priority: P2)

**Goal**: A written evaluation (`ci-cd-evaluation.md`) of
`.github/workflows/rust.yml` — what each existing step catches, named
gaps, and scoped recommendations tied to specific Story 1 findings.

**Independent Test**: Read `ci-cd-evaluation.md`; every existing step is
described and every recommendation states its cost and cites the finding
or gap that motivates it, with no pipeline file changed yet. Satisfies
spec SC-004.

### Implementation for User Story 2

- [X] T011 [US2] Document each existing step in `.github/workflows/rust.yml` (forbidden-path check, `cargo fmt --check`, `cargo clippy`, `cargo test`) as `existing` `CI Gate` rows in `specs/001-ci-cd-security-hardening/ci-cd-evaluation.md`, stating what each catches and what it explicitly does not
- [X] T012 [US2] For each Story 1 finding that an automated CI check could have caught (e.g., the `cargo audit` results from T007), add a `recommended` `CI Gate` row in `specs/001-ci-cd-security-hardening/ci-cd-evaluation.md` stating the added CI cost/time and the exact finding it addresses (depends on T005-T010 being complete)
- [X] T013 [US2] Review `specs/001-ci-cd-security-hardening/ci-cd-evaluation.md` against the `CI Gate` validation rules in `data-model.md` (every `recommended` row states `cost` and traces to a named finding/gap — no "more scanning is generally good" entries)

**Checkpoint**: `ci-cd-evaluation.md` is complete with a line-by-line adoptable recommendation list — Stories 1 and 2 together are a complete, mergeable "audit only" deliverable

---

## Phase 5: User Story 3 - Robustness Hardening (Priority: P3, optional continuation)

**Goal**: Every `critical`/`high` finding from Story 1 that's fixable
within this codebase is fixed with a passing regression test, or
explicitly recorded as an accepted risk.

**Independent Test**: For each finding marked `fixed`, its named
regression test fails on the pre-fix commit and passes after; the full
quality gate passes with zero regressions. Satisfies spec SC-002/SC-003.

### Implementation for User Story 3

- [X] T014 [US3] Triage every `critical`/`high` row in `specs/001-ci-cd-security-hardening/security-audit.md`: for each, decide fix-in-repo vs. `accepted-risk` (e.g., an unpatchable upstream CVE, or a fix the constitution's Principle IV/V would rule out) and note the decision inline (depends on T010) — no critical/high findings existed (see Summary in security-audit.md); operator instead chose 6 of the 12 low-severity findings to fix (SEC-002, 004, 006, 009, 011, 012), which is what T015-T019 below actually cover
- [X] T015 [P] [US3] Auth-area fix: SEC-002 (`src/config.rs` token min length 16→32). Regression tests in `config::tests`.
- [X] T016 [P] [US3] Input-validation-area fixes: SEC-011 (new `tests/submit.rs` coverage) surfaced SEC-013 (`src/error.rs` 413-vs-400 bug), fixed immediately; SEC-012 (`src/handlers.rs` `unreachable!()` → graceful error, no test — unreachable by construction, noted in security-audit.md).
- [X] T017 [P] [US3] File-path-area fixes: SEC-006 (`src/backup.rs` atomic write via `paths::tmp_sibling`) and SEC-009 (new `tests/purge.rs` + `backup.rs` inline tests).
- [X] T018 [US3] Updated `specs/001-ci-cd-security-hardening/security-audit.md`: all 7 addressed findings set to `status: fixed` (naming their regression tests, or explaining why none exists for SEC-012); added SEC-013 for the newly-discovered bug; deferred SEC-005/SEC-007 remain `open` with rationale.
- [X] T019 [US3] Ran `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test` — 113 passed, 0 failed (up from 101 at the T004 baseline), zero regressions.

**Checkpoint**: All fixable critical/high findings are fixed and tested; the rest are explicitly triaged — spec SC-002/SC-003 satisfied

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Final validation and any operator-approved follow-through

- [X] T020 Ran through `specs/001-ci-cd-security-hardening/quickstart.md` end to end — Story 1/2 reports reviewable, Story 3 quality gate green; see security-audit.md Summary for the honest caveats on which fixes have a strict fail-before/pass-after regression test vs. added coverage.
- [X] T021 SEC-013's fix changed a response shape (413 instead of 400 for oversized uploads, new `payload_too_large` error code) — updated `docs/API_CONTRACT.md`'s error-code list and Limits line in the same change.
- [X] T022 Operator approved the `cargo audit` recommendation. Added an "Install cargo-audit" (`taiki-e/install-action@v2`) + "cargo audit" step to `.github/workflows/rust.yml`, right after the existing cache step. `ci-cd-evaluation.md`'s row updated to `status: existing`, `decision: adopt`.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: No dependency on Setup content, but do it before Phase 5 (Story 3)
- **User Story 1 (Phase 3)**: Depends only on Setup (T001) — the MVP
- **User Story 2 (Phase 4)**: Depends on User Story 1 being complete (T012 cites its findings) — not independent of US1 despite the template's usual assumption
- **User Story 3 (Phase 5)**: Depends on User Story 1 (findings to fix) and Phase 2 (clean baseline) — optional continuation
- **Polish (Phase 6)**: Depends on whichever of Stories 1-3 were completed

### Parallel Opportunities

- T002 and T003 (Setup) can run in parallel with each other and with T001
- T015, T016, T017 (Story 3, per-area fixes) can run in parallel with each other once T014 triage is done — they touch disjoint files (`tests/auth.rs`+`src/auth.rs`/`src/config.rs`; `tests/submit.rs`/`tests/images.rs`+`src/handlers.rs`/`src/inputs.rs`; path-related `tests/*.rs`+`src/paths.rs` etc.)
- T005-T009 (Story 1, per-area audits) all write to the same `security-audit.md` file, so treat them as sequential even though they're conceptually independent research — avoid concurrent edits to one file

---

## Parallel Example: User Story 3

```bash
# After T014 triage is complete, launch per-area fix tasks together:
Task: "Fix auth findings: tests/auth.rs + src/auth.rs / src/config.rs"
Task: "Fix input-validation findings: tests/submit.rs or tests/images.rs + src/handlers.rs / src/inputs.rs"
Task: "Fix file-path findings: relevant tests/*.rs + src/paths.rs / src/db.rs / src/backup.rs / src/purge.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T003)
2. Complete Phase 3: User Story 1 (T005-T010)
3. **STOP and VALIDATE**: Review `security-audit.md` independently — this is a complete, mergeable deliverable even if nothing else in this feature proceeds

### Incremental Delivery

1. Setup → Story 1 (audit report) → review/merge as "audit only" if that's all that's wanted
2. Add Story 2 (CI evaluation) → review/merge — operator can adopt CI recommendations later, separately (T022)
3. Add Foundational baseline + Story 3 (fixes) → full quality gate green → review/merge
4. Polish (quickstart validation, contract-doc updates if needed)

## Notes

- [P] tasks touch different files and have no unmet dependency
- Stories 1 → 2 → 3 are dependency-ordered, not independent, because this
  feature is itself an audit-then-fix workflow — Story 2 needs Story 1's
  findings to cite, and Story 3 needs Story 1's findings to fix
- Story 3's exact task count depends on what Story 1 actually finds; T015-T017
  are written as "per area, if any" so they degrade to no-ops cleanly if a
  given area has no fixable critical/high finding
- Commit after each phase checkpoint, not after every single task
- Do not read or log real job input/output file contents or logs while
  auditing `src/paths.rs`/`src/inputs.rs`/`src/comfy.rs` — check code and
  metadata only (constitution Principle VI / spec FR-008)
