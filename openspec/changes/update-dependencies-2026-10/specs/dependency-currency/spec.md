# Spec Delta

## Purpose

Defines what "current" means for the server's dependencies and CI actions, and how an update is verified before release.

## ADDED Requirements

### Requirement: Direct dependencies are on their latest stable release
Every crate in `[dependencies]` and `[dev-dependencies]` SHALL be on its latest stable (non-prerelease) release at the time of the update, with the `Cargo.toml` floor equal to the version `Cargo.lock` resolves. The only exception is a crate recorded under Follow-ups in the change's design.md, with the reason it is held back.

#### Scenario: Audit after the update
- **WHEN** each crate in `Cargo.toml` is compared with its newest non-prerelease version on crates.io
- **THEN** every crate matches, except those listed as held back with a reason

### Requirement: CI actions are on their latest major
Every GitHub Action in `.github/workflows/` SHALL reference its latest major version tag.

#### Scenario: Checkout action
- **WHEN** `.github/workflows/rust.yml` is read
- **THEN** it uses `actions/checkout@v7`

### Requirement: Updates pass every quality gate without behaviour change
A dependency update SHALL pass `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` and `cargo audit`, with no test removed or weakened. The HTTP surface SHALL stay identical apart from the version string.

#### Scenario: Gates after the update
- **WHEN** the four gate commands run on the updated branch
- **THEN** all exit 0, and the test count is not lower than before the update

#### Scenario: Deployed smoke test
- **WHEN** the released binary is deployed and one job is submitted for each enabled workflow
- **THEN** each job reaches `done`, and its result download returns 200
