# zun-rust-server

A personal Rust server wrapping [project-zun](https://github.com/grand-omega/project-zun)'s ComfyUI + FLUX2 setup for a single-user Android client. Handles job orchestration, persistence, and an HTTP API the app drives.

Single-user, self-hosted. Plain HTTP backend designed to live behind a reverse proxy (Caddy, nginx, Tailscale Serve, etc.) that terminates TLS.

## Status

**v3.0.0** — current stable release. Major version tracks the Android client: 3.x clients pair with the 3.0 server. Plain HTTP behind a reverse proxy; multi-tenant scaffolding (per-IP rate limiter, proactive health probe, request-ID propagation) intentionally absent — see `docs/API_CONTRACT.md` for the surface. Verified against FLUX2 klein (~7 s per job on RTX 4070 Ti Super).

## Quick start

Prerequisites:
- Rust stable via [`rustup`](https://rustup.rs/)
- ComfyUI running from `zun-flux-pipeline` (`just serve` there)

Dev (from the repo root, finds `./config.toml`):

```bash
cp config.example.toml config.toml   # then edit: set token, bind address
cargo run                            # creates data/jobs.db with the v2 schema
```

Installed (binary anywhere, config anywhere):

```bash
cargo install --path .
zun-rust-server --config /etc/zun/config.toml
```

Relative paths in `config.toml` (`data_dir`) are resolved against the
config file's parent directory, not the current working directory — a
`cargo install`'d binary works the same regardless of where you run it
from.

Hit `/api/v1/health` to verify:

```bash
curl -s localhost:8080/api/v1/health | jq
# { "status": "ok", "version": "3.0.0", "disk": { "data_bytes": 0 } }
```

## Configuration

All config lives in `config.toml` (gitignored). Copy from
`config.example.toml`, which carries an annotated DEV/PROD profile pair.
The full reference table lives in that file's `=== Reference ===`
section; in short:

| Key | Default | Purpose |
|---|---|---|
| `token` | — (required) | Bearer token for the Android client |
| `bind` | `127.0.0.1:8080` | Listen address — server speaks plain HTTP |
| `comfy_url` | `http://127.0.0.1:8188` | ComfyUI HTTP base |
| `data_dir` | `./data` | Houses `jobs.db` and `{cache,outputs,thumbs,previews}/` |
| `log_format` | `auto` | `auto` (pretty on TTY, JSON otherwise), `pretty`, or `json` |
| `comfy_data_dir` | — (unset) | ComfyUI's dir holding `input/`+`output/`; lets the purge task clean up the `zun_*` files this server leaves there |

Workflows are baked into the binary at compile time — there is no
runtime knob to swap them. To update templates, edit the files under
`workflows/` and rebuild.

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

Workflow templates are vendored at `workflows/` (sibling of `src/`) and
baked into the binary at compile time via `include_dir!`. The set
exposed at runtime is gated by `ENABLED_WORKFLOWS` in
`src/workflow.rs` — adding a new pipeline is a code change, not a
config change. To update, edit the files under `workflows/` (or copy
new ones in from the authoring repo) and `cargo build`.

## Architecture

- **axum 0.8** HTTP server on tokio
- **sqlx + SQLite** (WAL) for the job queue — no external DB
- **reqwest (rustls)** to ComfyUI — pure Rust, no OpenSSL
- Background **worker** drains the queue one job at a time; per-prompt timeout; crash recovery on restart
- **tracing + tower-http** for structured logs and header redaction

## Security

The server speaks plain HTTP and assumes a reverse proxy in front terminates TLS and gates network access (firewall, overlay-network membership, Caddy `@allowed` matchers, whatever fits your topology). The bearer token (`config.toml: token`) is the application-layer second factor. There is no in-server rate limiting; if you need brute-force protection on the token, add it at the proxy.

## Roadmap

- **M8**: systemd unit for autostart on boot. `deploy/` is the placeholder;
  `main.rs` already supervises its background tasks on the assumption that
  something restarts the process when it exits non-zero.
- **M9**: FLUX.1 Fill / LoRA workflow support. The templates are already
  vendored under `workflows/`; wiring one up means adding it to
  `ENABLED_WORKFLOWS` and teaching the worker its extra placeholders.

WebSocket progress and the nightly cleanup task, previously listed under M9,
shipped in v3.0.0 — see `jobs.progress` plus `GET /jobs/{id}?wait=` and
`src/purge.rs`.
