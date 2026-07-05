<!--
Sync Impact Report
- Version change: 1.0.0 → 1.1.0
- Modified principles: none (purely additive amendment)
- Added sections:
  - Core Principles: VI. No Inspection of Sensitive Job/Log Content
- Removed sections: none
- Templates requiring updates:
  - ✅ .specify/templates/plan-template.md (Constitution Check gate is generic,
    references this file dynamically — no edit needed)
  - ✅ .specify/templates/spec-template.md (no constitution-specific references)
  - ✅ .specify/templates/tasks-template.md (no constitution-specific references)
  - ✅ CLAUDE.md (no conflicting guidance; left as-is per project convention of
    not editing files beyond what's requested)
- Follow-up TODOs: none

Prior report (v1.0.0):
- Version change: (none) → 1.0.0
- Modified principles: n/a (initial ratification)
- Added sections:
  - Core Principles: I. Single-User Simplicity, II. Surgical Changes, III. Quality
    Gates Are Non-Negotiable, IV. Explicit Security Boundary, V. Compile-Time
    Configuration Over Runtime Flexibility
  - Technology Constraints
  - Development Workflow
  - Governance
- Removed sections: none (template placeholders only)
- Templates requiring updates:
  - ✅ .specify/templates/plan-template.md (Constitution Check gate is generic,
    references this file dynamically — no edit needed)
  - ✅ .specify/templates/spec-template.md (no constitution-specific references)
  - ✅ .specify/templates/tasks-template.md (no constitution-specific references)
  - ✅ CLAUDE.md (already aligned with Principles I–II; left as-is per project
    convention of not editing files beyond what's requested)
- Follow-up TODOs: none
-->

# zun-rust-server Constitution

## Core Principles

### I. Single-User Simplicity
This server has exactly one tenant: a single Android client operated by its
owner. Multi-tenant scaffolding (per-IP rate limiting, tenant isolation,
proactive health probing for external consumers, configurable auth schemes)
MUST NOT be added speculatively. If a feature only matters for multiple
users or untrusted clients, it does not belong here until that requirement
is real. When evaluating "do we need X," the default answer is to remove or
simplify, not to add a toggle.

**Rationale**: The project was deliberately simplified by stripping
multi-tenant scaffolding once it was confirmed unnecessary (see git history:
"Strip multi-tenant scaffolding from the single-user server"). Re-adding
that complexity without a concrete driving need repeats a mistake already
corrected.

### II. Surgical Changes
Changes MUST be scoped to what the task requires. Do not refactor adjacent
code, reformat unrelated lines, or "improve" working code as a side effect
of an unrelated change. Match existing style even when a contributor would
personally choose differently. Pre-existing dead code or unrelated issues
MUST be reported, not silently fixed or deleted, unless the task explicitly
covers them. Every changed line should trace directly to the request that
motivated it.

**Rationale**: Small, traceable diffs are the only way a single maintainer
can review AI-assisted and human-assisted changes with confidence. This
mirrors the project's existing CLAUDE.md guidance and MUST be treated as
binding, not aspirational.

### III. Quality Gates Are Non-Negotiable
Every change MUST pass, before being committed:
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

These are enforced by the pre-commit hook and MUST NOT be bypassed
(`--no-verify` or equivalent) except with explicit, informed user
instruction for that one commit. Bug fixes and new endpoints MUST come with
a test that fails before the fix/feature and passes after; "fix the bug"
means "write a reproducing test, then make it pass."

**Rationale**: With one maintainer and an AI-assisted workflow, automated
gates are the primary defense against regressions slipping into a
production job queue that handles real GPU work and user data.

### IV. Explicit Security Boundary
The server speaks plain HTTP and trusts its network position: a reverse
proxy terminates TLS and gates network access, and a single bearer token
(`config.toml: token`) is the only application-layer credential. Any change
that touches auth, request parsing, or the job/data directories MUST
preserve this boundary explicitly rather than assuming defense-in-depth
that does not exist (no in-server rate limiting, no per-request IP
allowlisting, no session state beyond the bearer check). If a change needs
stronger protection than "proxy + one token," that requirement MUST be
surfaced to the user, not silently patched over in-process.

**Rationale**: The threat model is written down in README's Security
section and is intentional, not an oversight. Code changes must not
quietly expand or shrink that boundary.

### V. Compile-Time Configuration Over Runtime Flexibility
Workflow templates (`workflows/*.json`) are vendored and baked into the
binary at compile time via `include_dir!`; the set exposed at runtime is
gated by `ENABLED_WORKFLOWS` in code. Adding or changing a pipeline is a
code change plus rebuild, not a config file edit. New configurability MUST
NOT be introduced (config knobs, feature flags, env-var overrides) unless
the task explicitly calls for runtime flexibility — the default is to keep
behavior fixed at build time for a single deployment.

**Rationale**: This is a deliberate, already-established project convention
(see README "Developing" and "Configuration" sections) that trades runtime
flexibility for a smaller, more auditable surface on a single-operator
system.

### VI. No Inspection of Sensitive Job/Log Content
When working in this repository — debugging, testing, or reviewing — the AI
agent MUST NOT open, print, or otherwise inspect the contents of job
input/output files, intermediate artifacts, or log entries where doing so
would reveal a user's prompt text or other user-identifying information.
Checking file existence, size, path, or non-content metadata is fine;
payload contents (prompts, generated outputs, raw request/response bodies
carrying such data) are off-limits, even when troubleshooting a bug directly
tied to that content. If diagnosing an issue seems to require reading such
content, the agent MUST stop and ask the user instead of reading it
directly.

**Rationale**: This is a single-user system handling one real person's
generative-AI job data (see Principle I). Treating job payloads and logs as
freely inspectable debug artifacts would normalize a leak vector — the risk
is not the trusted current user, but agent-assisted work (pasted terminal
output, shared logs, future context) becoming the path by which prompt or
personal data escapes.

## Technology Constraints

- Language/toolchain: Rust stable via `rustup`, edition 2024.
- HTTP/async: `axum` on `tokio`.
- Persistence: `sqlx` + SQLite (WAL mode) — no external database service.
- Outbound HTTP to ComfyUI: `reqwest` with `rustls` — no OpenSSL dependency.
- Observability: `tracing` + `tower-http` for structured logs; sensitive
  headers (e.g. `Authorization`) MUST remain redacted in logs.
- New dependencies MUST be justified against this existing stack; prefer
  extending what's already vendored over introducing a parallel library for
  the same purpose (e.g. do not add a second HTTP client, ORM, or async
  runtime).

## Development Workflow

- Local iteration: `cargo run` (dev) / `cargo run --release` (prod-like),
  using `./config.toml` derived from `config.example.toml` via `just setup`.
- Commit gate (see Principle III): fmt, clippy with warnings-as-errors, and
  the full test suite must pass before a commit lands.
- Feature work merges through a `dev` branch into `main` via pull request,
  matching the repository's existing history; direct pushes to `main` for
  non-trivial changes are discouraged in favor of a reviewable PR.
- User-facing behavior changes (new endpoints, config keys, response
  shapes) MUST be reflected in `docs/API_CONTRACT.md` and/or `README.md` in
  the same change, since the Android client is a separate codebase that
  relies on this contract staying accurate.

## Governance

This constitution supersedes ad hoc practice for anything it explicitly
addresses; where it is silent, `CLAUDE.md`'s general LLM-collaboration
guidelines apply. Amendments require:
1. A stated reason (what changed, why the prior principle no longer fits).
2. An update to this file with a version bump per semantic versioning:
   - MAJOR: a principle is removed or redefined incompatibly.
   - MINOR: a new principle or section is added, or existing guidance is
     materially expanded.
   - PATCH: wording, clarification, or typo fixes with no rule change.
3. A check that `.specify/templates/*.md` still align (most gates in this
   repo's templates reference this file dynamically and need no edits;
   verify this remains true after structural changes).

Every plan and non-trivial change should be checked against these
principles before implementation, not after. Complexity that conflicts with
Principle I or V must be justified explicitly in the change's description,
not left implicit.

**Version**: 1.1.0 | **Ratified**: 2026-07-03 | **Last Amended**: 2026-07-05
