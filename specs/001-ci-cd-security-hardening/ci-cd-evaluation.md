# CI/CD Evaluation: zun-rust-server

**Date**: 2026-07-05
**Scope**: `.github/workflows/rust.yml` on `main`, per [spec.md](./spec.md) and [data-model.md](./data-model.md)

| Name | Status | Catches | Does Not Catch | Cost | Decision |
|---|---|---|---|---|---|
| Block forbidden paths (secrets/user data) | existing | Accidentally committed images, logs, `jobs.db`, real `config.toml`/`.env`, or anything under `data/` reaching the remote (runs `scripts/check-forbidden-paths.sh` against the full tree). | A secret pasted directly into an otherwise-allowed file (e.g. a token hardcoded into a `.rs` file) — the check is path-based, not content-based. | ~1s CI time; no external Action, just a repo script. | — |
| `cargo fmt --all -- --check` | existing | Formatting drift from the project's style. | Any logic or security issue — purely cosmetic. | ~2-5s CI time. | — |
| `cargo clippy --all-targets -- -D warnings` | existing | Common correctness footguns (`clippy::all`-level lints): some panic-prone patterns, obvious logic mistakes, dead code. | Known-vulnerable/unmaintained dependencies (SEC-001's class of finding); logic-level gaps like a missing timeout (SEC-004) or a non-atomic write pattern (SEC-006) — clippy has no lint for "this async call has no deadline" or "this write isn't atomic." | ~20-40s CI time (cached). | — |
| `cargo test` | existing | Regressions in already-tested behavior. | Untested paths — proven by this audit finding real coverage gaps (SEC-009, SEC-011) that `cargo test` passing gave no signal about. | ~1-2 min CI time (cached deps). | — |
| `cargo audit` (dependency vulnerability scan) | existing | Known-vulnerable (RUSTSEC advisory) dependencies and unmaintained/yanked-version warnings automatically on every push/PR — directly addresses SEC-001, which today requires a manual local run to notice. | Vulnerabilities not yet published to the RustSec advisory database (inherent to any advisory-based scanner, not specific to this tool). | ~10-30s CI time; installs the `cargo-audit` binary via `taiki-e/install-action` (fetches a prebuilt release, no extra workflow permissions needed) rather than a checks/issues-writing Action — no new Cargo dependency for the app itself. | adopt (2026-07-05) |
