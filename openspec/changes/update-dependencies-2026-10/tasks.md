# Tasks

## 1. Crates

- [x] 1.1 Raise the ten crates in `Cargo.toml` to the versions in the proposal table, then run `cargo update`. Verify: `cargo build --locked` succeeds and `git diff Cargo.toml` shows only those ten version changes.
- [x] 1.2 If `parking_lot` 0.13 breaks the build, adapt `src/state.rs` and `src/main.rs` mechanically. If more than that is needed, revert `parking_lot` to 0.12.5 and record the reason under Follow-ups in design.md. Verify: `cargo build --locked` succeeds.
- [x] 1.3 Run all quality gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` and `cargo audit`. Verify: all four exit 0, and the test count is not lower than the 137 at v3.2.0.
  - Result (clean worktree of the committed branch, 2026-10-27): all four exit 0. `cargo test --locked` reports 136 passed and 1 ignored (137 tests), the same as a clean worktree of v3.2.0. The working tree reports one more passing test because of the uncommitted `src/error.rs` change, which is not part of this change.

## 2. CI

- [ ] 2.1 In `.github/workflows/rust.yml`, change `actions/checkout@v5` to `actions/checkout@v7`. Verify: the PR's CI run passes.

## 3. Release

- [x] 3.1 Bump to 3.2.1 in `Cargo.toml`, `Cargo.lock`, `README.md` and `docs/API_CONTRACT.md`, in its own commit titled `Bump to 3.2.1`. Verify: `grep -rn "3\.2\.0" Cargo.toml README.md docs/API_CONTRACT.md` returns no version strings, and `cargo test --locked` passes.
- [ ] 3.2 **[operator]** Merge, then tag v3.2.1 on the dev-side commit and deploy on gpu-dev-4004 with backup and rollback. Verify: `/api/v1/health` reports `3.2.1`, `/capabilities` is unchanged apart from the version, and one `flux3_klein_edit` and one `flux2_klein_edit` job each reach `done` with a result download returning 200.

## Workflow follow-up

- Do not stage the pre-existing uncommitted change in `src/error.rs`.
- Archive with `openspec archive update-dependencies-2026-10` after release.
