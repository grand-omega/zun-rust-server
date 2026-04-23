# Changelog

## [0.1.0] — 2026-04-23

First tagged release. Feature-complete against the Android client's API contract; end-to-end tested against the real ComfyUI + FLUX2 klein (~7 s per job on an RTX 4070 Ti Super).

### API endpoints

- `GET  /api/health`               — unauth liveness; reports ComfyUI reachability.
- `GET  /api/prompts`              — prompt catalog (public projection).
- `POST /api/jobs`                 — multipart image + prompt_id → `{ job_id }`.
- `GET  /api/jobs/{id}`            — full status: `status`, `prompt_label`, `progress`, `error`, timestamps, dimensions.
- `GET  /api/jobs`                 — paginated list (`status`, `limit ≤100`, `before=<unix-s>`).
- `DELETE /api/jobs/{id}`          — removes DB row and on-disk files.
- `GET  /api/jobs/{id}/input`      — uploaded input, content-type by extension.
- `GET  /api/jobs/{id}/result`     — PNG output (409 until job is `done`).
- `GET  /api/jobs/{id}/thumb`      — 400 px JPEG, lazy-generated and cached.

All responses carry an `x-request-id` header; all authenticated endpoints require `Authorization: Bearer <token>`.

### Internals

- axum 0.8 + sqlx (SQLite, WAL) + reqwest (rustls, no OpenSSL).
- String-placeholder workflow substitution matching project-zun's contract.
- Background worker: one job at a time, per-prompt `timeout_seconds`, crash recovery (`running` → `queued` on startup), graceful shutdown on SIGINT/SIGTERM.
- Background ComfyUI health monitor, `/system_stats` every 30 s, audit events on healthy↔unhealthy transitions.
- Structured logging (pretty / JSON), request-id span on every event, `audit` target for lifecycle events (`job.submitted`/`running`/`done`/`failed`/`deleted`, `auth.denied`, `comfy.unreachable`/`recovered`).
- Pre-commit hook + GitHub Actions gate commits on `cargo fmt --check` + `cargo clippy -D warnings` + `cargo test`.

### Known limitations

- **No TLS** — plan's Milestone 7.
- **No systemd unit** — Milestone 8.
- **Only FLUX2 klein edit is fully wired.** FLUX.1 Fill, `_lora_` variants, and klein ref-edit have extra placeholder slots (`MASK_PROMPT_PLACEHOLDER`, `REFERENCE_IMAGE_PLACEHOLDER`, `LORA_PLACEHOLDER`) the server doesn't populate yet.
- **Single-user** — bearer-token auth only; multi-user would need a users table and a durable audit log.
- **No WebSocket progress** — `progress` field is always null; Android shows an indeterminate spinner.

### Coverage

66 tests (20 unit + 46 integration) — fmt/clippy clean.
