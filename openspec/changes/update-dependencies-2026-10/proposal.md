# Proposal

## Why

An audit on 2026-10-27 found 10 of the 28 crates and one GitHub Action behind their latest stable releases. All crate updates are patch or minor bumps. `parking_lot` 0.12 → 0.13 is a 0.x minor, which semver treats as potentially breaking. Unlike the Android app, this repo has no Renovate or Dependabot, so nothing proposes these updates automatically.

## What Changes

- **Crate updates in `Cargo.toml`:**

  | crate | from | to | gap |
  |---|---|---|---|
  | tokio | 1.53.1 | 1.54.1 | minor |
  | uuid | 1.26.0 | 1.29.0 | minor |
  | parking_lot | 0.12.5 | 0.13.0 | 0.x minor (breaking) |
  | reqwest | 0.13.4 | 0.13.5 | patch |
  | rand | 0.10.2 | 0.10.3 | patch |
  | sha2 | 0.11.0 | 0.11.1 | patch |
  | serde_json | 1.0.151 | 1.0.154 | patch |
  | thiserror | 2.0.20 | 2.0.21 | patch |
  | tokio-util | 0.7.19 | 0.7.20 | patch |
  | toml | 1.1.5 | 1.1.8 | patch |

- `Cargo.lock` is refreshed with `cargo update`, so transitive crates move to their latest compatible versions too.
- `.github/workflows/rust.yml`: `actions/checkout@v5` → `@v7`.
- Release as v3.2.1.

Non-goals:
- No new dependencies, removals or feature-flag changes.
- No Rust toolchain pin. CI keeps `dtolnay/rust-toolchain@stable`.
- No Renovate or Dependabot setup. The design records it as an option.
- No behaviour or API change. `/capabilities` and the route table stay identical apart from the version string.

## Capabilities

### New Capabilities
- `dependency-currency`: how the server keeps its direct dependencies and CI actions current and verified.

### Modified Capabilities
<!-- None. -->

## Impact

- **Code:**
  - `Cargo.toml`, `Cargo.lock`, `.github/workflows/rust.yml`.
  - Any call sites that `parking_lot` 0.13 breaks: `src/state.rs:3` and `src/main.rs:44` use `parking_lot::Mutex`.
  - The version string in `Cargo.toml`, `README.md` and `docs/API_CONTRACT.md`.
- **Runtime:** the release binary changes. Deploy to gpu-dev-4004 is an operator step, using the same backup and rollback pattern as the v3.2.0 deploy.
- **Clients:** none. The 3.x Android app needs no change.
