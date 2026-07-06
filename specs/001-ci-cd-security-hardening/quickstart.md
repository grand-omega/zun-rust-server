# Quickstart: Validating CI/CD Evaluation & Security Hardening

## Prerequisites

- Rust stable toolchain (edition 2024) installed
- `cargo install cargo-audit` (one-time, local machine — not required in
  CI until Story 2's recommendation, if adopted, adds it there)

## Story 1 — Security audit report

1. Run the dependency scan: `cargo audit` from repo root.
2. Review the areas named in `data-model.md`'s `Finding.area` enum by
   reading (not executing against real data) `src/auth.rs`, `src/paths.rs`,
   `src/inputs.rs`, `src/handlers.rs`, `src/comfy.rs`.
3. Confirm `security-audit.md` exists and every row has all `Finding`
   fields populated (spot-check against the table in `data-model.md`).
4. **Expected outcome**: a reviewable markdown report with no bare/vague
   findings — this alone satisfies SC-001 with no code changes required.

## Story 2 — CI/CD evaluation

1. Read `.github/workflows/rust.yml`.
2. Confirm `ci-cd-evaluation.md` lists every existing step (forbidden-path
   check, fmt, clippy, test) with what it catches / doesn't catch.
3. Confirm every `recommended` gate states a `cost` and traces to a
   specific Story 1 finding or named gap (data-model.md validation rules).
4. **Expected outcome**: a recommendation list the operator can accept or
   reject line-by-line — this satisfies SC-004 with no pipeline changes
   merged yet.

## Story 3 — Robustness fixes (optional continuation)

1. For each `Finding` marked `status: fixed`, locate its regression test
   in `tests/*.rs` and confirm it fails on a pre-fix checkout:
   ```
   git stash   # or check out the commit before the fix
   cargo test <test_name>   # expect FAIL
   git stash pop            # or return to the fix commit
   cargo test <test_name>   # expect PASS
   ```
2. Run the full quality gate:
   ```
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
3. **Expected outcome**: all three commands exit 0 (SC-003); every
   `critical`/`high` finding is `fixed` (with a passing regression test) or
   `accepted-risk` with a stated reason (SC-002).

## Out of scope for validation here

- `zun-android-app` and `zun-flux-pipeline` are not exercised by this
  quickstart (per spec Assumptions) — only their existing contract with
  this server (`docs/API_CONTRACT.md`) must remain unbroken, which
  `cargo test` already covers for this repo's side of that contract.
