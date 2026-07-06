# Phase 0 Research: CI/CD Evaluation & Security Hardening

## R1: Dependency vulnerability scanning

**Decision**: Add `cargo audit` (via `rustsec/audit-check` or an equivalent
pinned GitHub Action) as a new CI step, gated the same way the existing
`check` job is (runs on push/PR to `main`/`dev`).

**Rationale**: A live run against this repo's `Cargo.lock` (368 resolved
crates) found:
- 0 known-vulnerable (RUSTSEC advisory) dependencies today.
- 2 warnings: `paste 1.0.15` is unmaintained (RUSTSEC-2024-0436, pulled in
  transitively), and a yanked `num-bigint 0.4.7` version is present in the
  lockfile.

Zero live vulnerabilities today is exactly why this belongs in CI now,
before one appears silently between manual checks — it's a single `cargo
audit` invocation, no new dependency on the app itself, and fits the
project's existing "small number of fast, deterministic CI gates" pattern
(constitution Principle III).

**Alternatives considered**:
- `cargo deny` (advisories + license + ban-list + duplicate-version
  checks): broader than what's needed here; the project has no license-
  compliance requirement and adding a bans/duplicates policy is unrequested
  configurability (constitution Principle V). Rejected for this feature;
  `cargo audit` alone covers the actual finding.
- Dependabot / Renovate: passive PR-based nagging rather than a blocking CI
  gate, and adds an always-on bot to a single-maintainer repo. Rejected —
  doesn't match "quality gates are non-negotiable," it's opt-in triage.

## R2: Testing strategy for "robustness" findings

**Decision**: Cover malformed/hostile input via targeted `cargo test`
integration tests added to the existing `tests/*.rs` files (same pattern
already used for `auth.rs`, `submit.rs`, etc.) — e.g., oversized multipart
bodies, non-UTF8/path-traversal-shaped filenames, truncated/garbage image
bytes, missing/invalid bearer tokens, malformed JSON bodies. No new test
framework or fuzzing harness.

**Rationale**: The project already has an integration-test pattern with
`wiremock` (mocks the ComfyUI/pipeline boundary) and `tempfile` (isolated
`data_dir`). Reusing it keeps Story 3 fixes inside the existing "one
regression test per bug" discipline (constitution Principle III) instead
of introducing new tooling for a single-maintainer project.

**Alternatives considered**:
- `cargo fuzz` / `proptest`-based fuzzing: valuable for parser-heavy code,
  but this codebase's untrusted-input surface is narrow (multipart upload,
  JSON bodies, path/id strings already centralized through `paths.rs`).
  Adding a fuzzing harness is new tooling and CI time disproportionate to
  the actual attack surface — rejected per Principle V (no unrequested
  complexity) unless a specific finding demonstrates it's warranted.

## R3: Report format and location

**Decision**: Two plain markdown files under this feature's `specs/`
directory: `security-audit.md` (Story 1) and `ci-cd-evaluation.md` (Story
2), each following the `Finding` / `CI Gate` structures defined in
`data-model.md`.

**Rationale**: Matches how every other Spec Kit artifact in this repo is
stored (`specs/<feature>/*.md`); keeps the audit attached to the feature
that produced it and reviewable in the same PR as any Story 3 fixes.
Promoting durable findings into `README.md` or `docs/` is a separate,
explicit decision left to the operator (per constitution Principle II —
don't touch files beyond what the task requires).

**Alternatives considered**:
- GitHub Issues per finding: adds process overhead (one issue per finding)
  disproportionate to a single-maintainer workflow where the audit and the
  fix are likely to land in the same PR. Rejected.
- `docs/SECURITY_AUDIT.md` at repo root: would imply this is a
  permanently-maintained, continuously-updated document; the spec frames
  this as a one-time evaluation, not an ongoing living document. Rejected
  for now — the operator can promote it later if they want it kept current.

## R4: Clippy lint scope

**Decision**: Keep the existing `cargo clippy --all-targets -- -D
warnings` (effectively `clippy::all` + `clippy::correctness` etc. at deny
level) as the CI gate; do not add `clippy::pedantic` or `clippy::nursery`
as a blocking gate.

**Rationale**: `clippy::all` already catches real correctness and most
security-adjacent footguns (e.g. integer overflow patterns it flags,
unwraps in obviously-fallible spots via other lints the team already
relies on). `pedantic`/`nursery` are style/opinion lints with a high false-
positive-to-real-finding ratio and would require an allow-list to be
usable — that's exactly the kind of unrequested configurability Principle
V warns against for a single-maintainer repo. If the audit turns up a
specific pattern (e.g. a class of panic-prone `unwrap()`), that's handled
as a targeted Story 3 fix + regression test, not a blanket lint-level change.

**Alternatives considered**:
- `clippy::pedantic` at warn (non-blocking): produces noise without
  enforcement; rejected as neither useful nor free.
- A `#![deny(clippy::unwrap_used)]` crate-wide lint: too broad a hammer
  sight-unseen; only worth it if the audit finds a real pattern of
  panicking on attacker-controlled input, in which case it becomes a
  targeted Story 3 finding instead of a Phase 0 decision.
