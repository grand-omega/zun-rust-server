# zun-rust-server

A personal Rust server wrapping [project-zun](https://github.com/grand-omega/project-zun)'s ComfyUI + FLUX2 setup for a single-user Android client. Handles job orchestration, persistence, and an HTTP API the app drives.

Single-user, self-hosted. Runs on home LAN or Tailscale — no special network setup required.

## Status

**v0.2.0** — API-complete, end-to-end verified against real FLUX2 klein (~7 s per job on RTX 4070 Ti Super).

## Quick start

Prerequisites:
- Rust stable via [`rustup`](https://rustup.rs/)
- ComfyUI running from `project-zun` (`just serve` there)

```bash
cp config.example.toml config.toml   # then edit: set token, bind address
cargo run                            # creates data/jobs.db with the v2 schema
cargo run --bin zun-admin -- seed-prompts --from starter_prompts.toml
```

Hit `/api/v1/health` to verify:

```bash
curl -s localhost:8080/api/v1/health | jq
# { "status": "ok", "version": "0.1.0", "comfy": { "ok": true, ... } }
```

## Configuration

All config lives in `config.toml` (gitignored). Copy from `config.example.toml`:

| Key | Default | Purpose |
|---|---|---|
| `token` | — (required) | Bearer token for the Android client |
| `bind` | `0.0.0.0:8080` | Listen address — works on LAN and Tailscale simultaneously |
| `comfy_url` | `http://127.0.0.1:8188` | ComfyUI HTTP base |
| `data_dir` | `./data` | Houses `jobs.db`, `{cache,outputs,thumbs,previews}/`, and the `workflows/` symlink |
| `default_workflow` | `flux2_klein_edit` | Default workflow advertised to Android |
| `enabled_workflows` | `flux2_klein_edit`, `flux2_klein_9b_kv_experimental` | Explicit workflow names exposed by the server |
| `log_format` | `auto` | `auto` (pretty on TTY, JSON otherwise), `pretty`, or `json` |

`RUST_LOG` env var still works for log-level tuning (e.g. `RUST_LOG=debug`).

## Developing

```bash
cargo run              # debug build
cargo run --release    # release build
```

Commit gate (pre-commit hook):

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Workflow templates live in project-zun and are consumed via a symlink:

```bash
ln -s ../../project-zun/workflows data/workflows
```

## Architecture

- **axum 0.8** HTTP server on tokio
- **sqlx + SQLite** (WAL) for the job queue — no external DB
- **reqwest (rustls)** to ComfyUI — pure Rust, no OpenSSL
- Background **worker** drains the queue one job at a time; per-prompt timeout; crash recovery on restart
- Background **health monitor** probes ComfyUI every 30 s; state exposed on `/api/health`
- **tracing + tower-http** for request-ID spans, structured logs, header redaction

## Security

Network boundary is the primary auth layer. On Tailscale, only enrolled devices can reach the server. On home LAN, only devices on the local network. A bearer token (`config.toml: token`) provides a second layer.

## Roadmap

- **M8**: systemd unit for autostart on boot
- **M9**: FLUX.1 Fill / LoRA workflow support, WebSocket progress, nightly cleanup task

See `plan/PLAN.md` for the full architecture and milestone breakdown.
