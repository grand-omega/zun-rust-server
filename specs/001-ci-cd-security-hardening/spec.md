# Feature Specification: CI/CD Evaluation & Security Hardening

**Feature Branch**: `001-ci-cd-security-hardening`

**Created**: 2026-07-05

**Status**: Draft

**Input**: User description: "perform an evaluation on this repo's CI/CD, and do security audit, make this server robust"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Security Audit Report (Priority: P1)

As the operator of zun-rust-server (a single-user Axum service handling one
person's generative-AI job data), I want a prioritized report of security
weaknesses in the codebase — auth handling, input parsing, dependency
vulnerabilities, data-directory exposure, and the ComfyUI-facing HTTP
boundary — so I know exactly what to fix and in what order, without wading
through the whole codebase myself.

**Why this priority**: Without knowing what's actually wrong, CI/CD changes
and "robustness" work risk fixing the wrong things. The audit is the
prerequisite that makes every other story actionable.

**Independent Test**: Can be fully tested by producing a written audit
document listing each finding with severity, affected file/line, and a
recommended fix — deliverable and reviewable on its own, with no code
changes required to "pass."

**Acceptance Scenarios**:

1. **Given** the current codebase on `main`, **When** the audit is run,
   **Then** it produces a report covering at minimum: authentication/token
   handling, request input validation, dependency vulnerabilities (known
   CVEs in the `Cargo.lock` tree), file/path handling around `data_dir`,
   and outbound requests to `comfy_url`.
2. **Given** a finding in the report, **When** the operator reads it,
   **Then** the finding states severity (critical/high/medium/low), why it
   matters for this specific single-user/single-token threat model (per
   the project constitution's Principle IV), and a concrete remediation
   step.
3. **Given** the constitution's Principle IV (explicit security boundary:
   proxy + one bearer token, no in-app rate limiting/IP allowlisting),
   **When** a finding is raised, **Then** it does not recommend
   multi-tenant/defense-in-depth patterns that principle explicitly rules
   out, unless it argues explicitly that the boundary itself is insufficient.

---

### User Story 2 - CI/CD Pipeline Evaluation (Priority: P2)

As the operator, I want an assessment of the current CI pipeline
(`.github/workflows/rust.yml`) — what it catches today, what gaps exist
(e.g., no dependency-vulnerability scan, no security-focused lint pass) —
so I can decide which additional automated gates are worth adding for a
single-maintainer project.

**Why this priority**: CI is the automated backstop referenced by
Principle III of the constitution ("Quality Gates Are Non-Negotiable"). It
depends on Story 1's findings to know which gaps are worth closing, so it
follows the audit.

**Independent Test**: Can be fully tested by producing a written evaluation
of the existing workflow (what runs, what it would and wouldn't catch,
given Story 1's findings) with concrete, scoped recommendations — reviewable
without any pipeline changes being merged.

**Acceptance Scenarios**:

1. **Given** the current `rust.yml` workflow, **When** the evaluation is
   run, **Then** it lists each existing step (forbidden-path check, fmt,
   clippy, test) and states what class of problem each one does and does
   not catch.
2. **Given** a security finding from Story 1 caused by a known-vulnerable
   dependency, **When** the CI evaluation is produced, **Then** it
   recommends a specific, minimal automated check that would have caught
   it (e.g., a dependency-audit step), scoped to this single-maintainer
   repo (no heavyweight/multi-team tooling).
3. **Given** the evaluation is complete, **When** recommendations are
   listed, **Then** each one states the tradeoff (added CI time, new
   dependency on an external action/tool) so the operator can decide
   whether it's worth adopting, consistent with Principle V (avoid
   unrequested configurability/complexity).

---

### User Story 3 - Robustness Hardening (Priority: P3)

As the operator, I want the highest-severity, highest-confidence findings
from the security audit actually fixed in the code (not just reported), so
the server is measurably more resistant to malformed input, resource
exhaustion, and crashes once this feature is done.

**Why this priority**: Fixing code depends on Story 1 identifying real,
verified issues first; doing this before the audit risks fixing invented
problems or missing real ones.

**Independent Test**: Can be fully tested by taking each fixed finding,
confirming a regression test exists that reproduces the original issue and
passes only after the fix (per Principle III), and confirming
`cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` all
still pass.

**Acceptance Scenarios**:

1. **Given** a critical or high-severity finding from Story 1 that is
   within this server's own code (not an external dependency the operator
   doesn't control), **When** the fix is applied, **Then** a test exists
   that fails against the pre-fix code and passes after.
2. **Given** all P1 findings are addressed, **When** the full quality gate
   (`cargo fmt --check && cargo clippy --all-targets -- -D warnings &&
   cargo test`) is run, **Then** it passes.
3. **Given** a finding whose fix would require adding functionality the
   constitution rules out for a single-user system (e.g., per-IP rate
   limiting), **When** that finding is triaged, **Then** it is documented
   as an accepted risk with rationale rather than silently implemented.

### Edge Cases

- What happens when a dependency-vulnerability scan flags a CVE with no
  available upstream fix? → Documented as an accepted/tracked risk, not
  silently ignored or blocked on indefinitely.
- How does the audit handle findings that only matter for a multi-tenant
  deployment this project explicitly will never become (per constitution
  Principle I)? → Noted as "not applicable to this system's threat model"
  rather than flagged as a gap.
- What happens if a robustness fix (Story 3) would require an interface
  change the Android client (`zun-android-app`) depends on? → Flagged for
  the operator's explicit decision rather than changed silently, since that
  repo's `API_CONTRACT.md` reliance is out of scope for this feature to
  edit unilaterally.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The audit MUST produce a written report enumerating security
  findings across auth/token handling, input validation, dependency
  vulnerabilities, file/path handling, and the outbound ComfyUI/pipeline
  HTTP boundary.
- **FR-002**: Each finding MUST include a severity rating, the affected
  location (file/line or component), why it matters under this project's
  actual single-user/single-token threat model, and a recommended fix.
- **FR-003**: The audit MUST check the dependency tree (`Cargo.lock`) for
  known vulnerabilities and license/maintenance red flags.
- **FR-004**: The CI/CD evaluation MUST describe what the existing
  `rust.yml` pipeline does and does not catch, and MUST NOT recommend
  changes outside what a single-maintainer repo can realistically sustain
  (per constitution Principle V).
- **FR-005**: For each Story 3 fix applied, a regression test MUST exist
  that reproduces the original issue and passes only after the fix (per
  constitution Principle III).
- **FR-006**: Findings and recommendations MUST respect constitution
  Principle IV (explicit security boundary: proxy + one bearer token) —
  i.e., MUST NOT recommend reintroducing multi-tenant defense-in-depth
  patterns already deliberately excluded, unless explicitly arguing the
  boundary itself is inadequate.
- **FR-007**: All changed/added code from Story 3 MUST pass
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` before being considered done (per constitution Principle
  III).
- **FR-008**: The audit and its fixes MUST NOT read or log the contents of
  real job input/output files or logs containing user prompt data (per
  constitution Principle VI) — file existence/metadata checks are
  sufficient for evaluating file/path-handling risk.

### Key Entities

- **Finding**: A single audit result — severity, location, description,
  rationale tied to this system's actual threat model, recommended fix,
  and status (open / fixed / accepted risk).
- **CI Gate**: A single automated check in the pipeline (existing or
  recommended) — what it runs, what class of defect it catches, and its
  cost (time, new dependency).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The operator receives a single written audit report covering
  100% of the areas in FR-001 within one review session (no follow-up
  research needed to understand a finding).
- **SC-002**: Every finding rated critical or high that is fixable within
  this codebase (not an unpatchable upstream CVE) is either fixed with a
  passing regression test, or explicitly recorded as an accepted risk with
  a stated reason.
- **SC-003**: The full quality gate (fmt + clippy + test) passes after all
  Story 3 changes, with zero regressions in previously-passing tests.
- **SC-004**: The CI/CD evaluation results in a concrete, scoped
  recommendation list the operator can adopt or reject item-by-item — not
  a vague "add more security" statement.

## Assumptions

- The audit and hardening work is scoped to this repository
  (`zun-rust-server`) only; `zun-android-app` and `zun-flux-pipeline` are
  out of scope except where this server's API contract with them must be
  preserved.
- This is engineering/operational work, not an end-user-facing feature —
  "user" in the stories above refers to the repo's sole operator, not the
  Android app's end user, consistent with this being a single-user system
  (constitution Principle I).
- The audit runs against the current `main` branch state at time of
  execution.
- Findings that only matter for multi-tenant or internet-exposed-without-
  proxy deployments are out of scope, since the constitution rules those
  deployment shapes out.
- No new runtime configurability (feature flags, env vars) will be
  introduced to remediate findings unless a finding specifically requires
  it and that requirement is surfaced explicitly (constitution Principle V).
