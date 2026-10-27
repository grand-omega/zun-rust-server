# Design

## Context

The audit was read-only, against `origin/main` at v3.2.0. It covered `Cargo.toml` and CI, and confirmed that `Cargo.lock` resolves exactly to the `Cargo.toml` floors. 18 of the 28 crates and the other CI actions (`Swatinem/rust-cache@v2`, `taiki-e/install-action@v2`) are already current.

The constitution requires every change to pass `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` and `cargo audit` (quality gates). It also says new dependencies must be justified, and this change adds none.

## Goals / Non-Goals

**Goals:**
- Every direct crate is on its latest stable release.
- CI actions are on their latest major.
- All quality gates are green.
- Nothing changes in behaviour.

**Non-Goals:**
- Toolchain pinning.
- Automated dependency PRs.
- Dependency pruning.

## Decisions

1. **Raise the `Cargo.toml` floors to the new versions, not just `cargo update`.**
   - The repo's convention is that the floors equal the lock, as the audit found.
   - A plain `cargo update` would leave the floors stale and make the next audit misleading.
2. **`parking_lot` 0.13 is in scope but isolated.**
   - Only `Mutex` is used.
   - If 0.13 changes that API, adapt the two call sites.
   - If it needs more than a mechanical change, hold it at 0.12.5 and record why under Follow-ups rather than rewriting locking code.
3. **`actions/checkout` goes straight to v7**, the latest major. The workflow only uses the default fetch, so neither of the two majors in between should affect it.
4. **Release as a patch (v3.2.1).** No API or behaviour change. The version-bump commit follows the v3.1.0 and v3.2.0 pattern: `Cargo.toml`, `Cargo.lock`, `README.md` and `docs/API_CONTRACT.md`.
5. **Renovate is not added here.** The Android app already uses Renovate with a weekly schedule. Adding the same `renovate.json` here would be a separate, one-file change, so it is noted as a follow-up for the owner to decide.

## Risks / Trade-offs

- **Transitive updates from `cargo update` change runtime behaviour, for example in hyper, rustls or sqlx.**
  - Mitigations: the full integration test suite covers the HTTP surface, `cargo audit` catches advisories, and the operator does a post-deploy smoke test.
- **checkout v7 drops something the workflow relies on.**
  - Mitigation: the CI run on the PR shows it.

## Migration Plan

1. Merge to dev, then make a release PR to main and tag v3.2.1 on the dev-side commit.
2. **[operator]** Deploy on gpu-dev-4004 with the v3.2.0 deploy pattern (back up the binary, build, restart, verify, roll back on failure).
3. **Rollback:** restore the backed-up binary and restart.

## Follow-ups

- Optional: add `renovate.json`, mirroring the Android app's, so crate updates arrive as weekly PRs.
- Task 2.1 is pending verification. `.github/workflows/rust.yml` now uses `actions/checkout@v7`, the latest major (v7.0.1), and the file parses as YAML. The task's Verify, a passing CI run on the PR, can only happen once the branch is pushed and a PR is opened, so 2.1 stays unchecked until then.
