# zun-rust-server

A personal Rust server wrapping [project-zun](https://github.com/grand-omega/zun-rust-server/../project-zun)'s ComfyUI + FLUX2 setup for a single-user Android client. Handles job orchestration, persistence, and an HTTP API the app drives.

Single-user, self-hosted, designed to sit behind Tailscale. Deployed as a single static binary.

## Status

**v0.1.0** — API-complete for the Android client. End-to-end works against real FLUX2 klein (~7 s per job on RTX 4070 Ti Super). See [`CHANGELOG.md`](CHANGELOG.md) for scope, [`plan/PLAN.md`](plan/PLAN.md) for the full architecture, and [`ANDROID_TESTING.md`](ANDROID_TESTING.md) for client-integration notes.

## Quick start

Prerequisites:
- Rust stable via [`rustup`](https://rustup.rs/)
- ComfyUI running from `project-zun` (`just serve` there)

```bash
export ZUN_TOKEN=$(openssl rand -hex 32)   # any ≥16 char string
cargo run --release
```

Server listens on `127.0.0.1:8080`. Hit `/api/health` to check:

```bash
curl -s localhost:8080/api/health | jq
# { "status": "ok", "version": "0.1.0", "comfy": { "ok": true, ... } }
```

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `ZUN_TOKEN` | — (required) | Bearer token for the Android client |
| `ZUN_BIND` | `127.0.0.1:8080` | Listen address |
| `ZUN_DATA_DIR` | `./data` | Houses `jobs.db`, `inputs/`, `outputs/`, `thumbs/`, `workflows/`, `prompts.yaml` |
| `ZUN_COMFY_URL` | `http://127.0.0.1:8188` | ComfyUI HTTP base |
| `ZUN_LOG_FORMAT` | auto (pretty on TTY, JSON otherwise) | Log output format |
| `RUST_LOG` | `zun_rust_server=info,tower_http=info,audit=info` | Log filter |

## Developing

Commit gate (pre-commit hook mirrors CI):

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

One-time hook setup (untracked):

```bash
cp scripts/pre-commit .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit
```

(Or just use the one I've had locally — see `.git/hooks/pre-commit` if you're on my workstation.)

Workflow templates live in project-zun and are consumed via a symlink:

```bash
ln -s ../../project-zun/workflows data/workflows
```

## Architecture at a glance

- **axum 0.8** HTTP server on tokio.
- **sqlx + SQLite** for the job queue (WAL, no Postgres).
- **reqwest (rustls)** to ComfyUI — pure Rust TLS, no OpenSSL.
- Background **worker** drains the queue one job at a time through ComfyUI; per-prompt timeout; graceful shutdown on SIGTERM.
- Background **health monitor** probes ComfyUI's `/system_stats` every 30 s; state exposed on `/api/health`, transitions emit audit events.
- **tracing + tower-http** for request-id spans, structured logs, and header redaction.

## Roadmap

- **M7**: Tailscale IP bind + `tailscale cert` rustls termination.
- **M8**: systemd unit, `/etc/zun/env`, log-rotation via journald.
- **M9**: FLUX.1 Fill / LoRA workflow support, optional WebSocket progress, nightly cleanup.

See `plan/PLAN.md` for the full milestone breakdown.
