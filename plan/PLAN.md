# zun-rust-server — Development Plan

> A personal Rust server wrapping ComfyUI (via [project-zun](../project-zun))
> for an Android client. Single-user, self-hosted behind Tailscale.
> Deployed as a single static binary via systemd.
>
> Models: FLUX 2 klein (daily driver) plus FLUX.1 Fill and future workflows.
> Pure-Rust dependency posture (rustls everywhere, no system libs) so the
> build is OS-independent — dev is on Gentoo, deploy target is generic Linux.

---

## Table of contents

1. [Overview and scope](#overview-and-scope)
2. [Architecture](#architecture)
3. [Tech stack](#tech-stack)
4. [Security model](#security-model)
5. [Data model](#data-model)
6. [API contract](#api-contract)
7. [ComfyUI integration](#comfyui-integration)
8. [Job lifecycle and worker](#job-lifecycle-and-worker)
9. [File and storage layout](#file-and-storage-layout)
10. [Thumbnail generation](#thumbnail-generation)
11. [Prompts configuration](#prompts-configuration)
12. [Error handling](#error-handling)
13. [Project structure](#project-structure)
14. [Build order and milestones](#build-order-and-milestones)
15. [Testing approach](#testing-approach)
16. [Deployment](#deployment)
17. [Operational concerns](#operational-concerns)

---

## Overview and scope

zun-rust-server is a thin orchestration layer sitting between the Android client and an already-working ComfyUI setup maintained in the sibling [project-zun](../project-zun) repository. It accepts image edit requests, manages a job queue, shells out to ComfyUI via its HTTP API, and serves results back. It is not an ML runtime itself — ComfyUI owns that.

### Responsibilities

- Accept image uploads over HTTPS with bearer-token auth
- Persist job metadata in SQLite
- Orchestrate ComfyUI job execution via its HTTP API
- Generate thumbnails of outputs
- Serve job status, results, inputs, thumbnails, and history to the client
- Expose a configurable catalog of predefined prompts, each bound to a specific workflow template

### Workflow families supported

The Python side (project-zun) ships ~10 workflow JSON templates across two model families:

- **FLUX 2 klein** — fast, maskless image-to-image edits. `flux2_klein_edit.json` is the **primary / daily driver for v1**.
- **FLUX.1 Fill** — mask-based inpainting (GDINO+SAM auto-masking). Available but slower; wired in after the klein path is solid.
- Future workflows (klein t2i, ref-edit, new model families) are expected. The server's design treats workflows as opaque templates keyed by filename; adding a new one should be a prompts.yaml edit, not a code change.

### Deliberate non-goals

- **No multi-user support.** Single bearer token, single user.
- **No public-internet exposure.** Server binds only to its Tailscale interface.
- **No ML inference in-process.** ComfyUI runs as a separate service.
- **No user-facing web UI.** The Android app is the only client.
- **No queue-like prioritization.** Jobs are processed in FIFO order.
- **No horizontal scaling concerns.** Single process, single machine, single GPU.
- **No LoRA support in v1.** project-zun trains LoRAs (`outfit_klein_v1`, `reimu_v1`) and workflows contain a `LORA_PLACEHOLDER` marker, but the server ignores this field for v1. Revisit once the core job loop is stable.
- **No text-to-image workflows in v1.** `POST /api/jobs` requires an input image. klein t2i and other "no input image" workflows are out of scope until we extend the API shape.

### Target deployment

A Linux server (Ubuntu 24.04 or similar) with a GPU capable of running FLUX2 via ComfyUI. Server reachable from the developer's Android phone via Tailscale. Deployed as a systemd service; configured via a small TOML file plus environment variables for secrets.

### Dev-time defaults (kickoff decisions)

Conventions for local development; prod overrides come from `config.toml`.

- **Data directory:** `./data/` inside this repo (gitignored). Houses `jobs.db`, `inputs/`, `outputs/`, `thumbs/`, `workflows/`, `prompts.yaml`.
- **Workflows directory:** `./data/workflows/` is a **symlink** to `../project-zun/workflows/` — project-zun is the source of truth; edits to a workflow JSON propagate without a copy step. Setup: `ln -s ../../project-zun/workflows data/workflows`.
- **Prompts file:** `./data/prompts.yaml` committable with **placeholder dev prompts** (not gitignored in dev). User swaps in real "secret" prompts later; at that point we flip the gitignore.
- **ComfyUI URL:** `http://127.0.0.1:8188` (project-zun's `just serve` default).
- **Bind address:** `127.0.0.1:8080` for dev (plain HTTP). Tailscale IP + TLS come at M7.
- **Android client absent.** API correctness is verified entirely via Rust integration tests (`tests/*.rs` using `tower::ServiceExt::oneshot`) and `scripts/test.sh` curl smoke tests. No client-side contract review possible yet — the server is the spec.
- **Progress reporting:** not in v1. `GET /api/jobs/{id}` always returns `progress: null`. Android will get real progress when we add the WebSocket bridge in a later milestone.
- **Image format on the wire:** outputs served as PNG as-is, thumbnails as 400 px JPEG. No format negotiation / transcoding until we have real mobile-bandwidth complaints.

---

## Architecture

### Process topology

```
+-------------------------------------------------------------+
|  Linux server (behind Tailscale, bound to 100.x.x.x)        |
|                                                             |
|  +-----------------------+      +------------------------+  |
|  | zun-server.service       |      | comfyui.service        |  |
|  | (Rust binary)         |----->| (Python, on 127.0.0.1) |  |
|  |                       | HTTP |                        |  |
|  | - axum HTTP server    |      | - FLUX2 workflow       |  |
|  | - tokio worker task   |      | - GPU inference        |  |
|  | - SQLite (file-based) |      |                        |  |
|  +-----------------------+      +------------------------+  |
|           |                                                 |
|           v                                                 |
|  /srv/zun/                                              |
|    |-- jobs.db       (SQLite)                               |
|    |-- inputs/       (uploaded images)                      |
|    |-- outputs/      (ComfyUI results)                      |
|    |-- thumbs/       (server-generated 400px thumbnails)    |
|    |-- workflows/    (ComfyUI workflow templates)           |
|    `-- prompts.yaml  (prompt catalog)                       |
+-------------------------------------------------------------+
```

### Process model inside the Rust binary

- **Main tokio runtime** hosting:
  - axum HTTP server (handles requests)
  - background worker task (drains queued jobs, calls ComfyUI, updates DB)
  - periodic cleanup task (optional; runs nightly, deletes old files)
- **Shared state** (`AppState`) holding DB pool, HTTP client, config, and a channel to wake the worker when a new job arrives.

### Client-server interaction model

- **Async jobs.** `POST /api/jobs` inserts a queued row and returns `{job_id}` immediately. The worker picks it up independently.
- **Polling-based status.** Client polls `GET /api/jobs/{id}` every 3 seconds until `done` or `failed`.
- **Authenticated static file serving.** Inputs, outputs, and thumbnails are served through authenticated endpoints, not as raw static files, so the bearer token is checked on every image fetch.

### Key design decisions

- **SQLite, not Postgres.** Single-writer workload, tiny scale, file-based. Zero operational burden. `sqlx` gives compile-time checked queries.
- **Integrated worker, not separate service.** No Celery, RQ, or sidecar. A tokio task inside the same binary reads queued jobs and processes them. Simpler to deploy and reason about.
- **ComfyUI workflow JSON as opaque templates.** Load every `workflows/*.json` at startup, clone per-job, patch via string-placeholder substitution (`PROMPT_PLACEHOLDER` etc.) — no hardcoded node IDs. Matches project-zun's existing pattern.
- **Bearer token auth only.** No OAuth, no JWTs, no sessions. Single long random string known to the app and the server.

---

## Tech stack

### Dependency posture: pure Rust, no system libs

Every dependency is built from source via cargo. No `pkg-config`, no OpenSSL, no `libssl-dev`. The resulting binary links only libc (and, if we want, we can go fully static with musl later). Rationale:

- **Portability.** Dev runs on Gentoo; deploy target is some other Linux. Anything that shells out to the host's OpenSSL will behave differently across distros (OpenSSL 1.1 vs 3.x has bitten many people). Pure Rust gives identical behavior everywhere.
- **Easy upgrades.** `cargo update` handles everything; no need to track distro package versions.
- **Cross-compile-ready.** If we ever need to target aarch64 or musl, a pure-Rust tree makes it a one-line change.

Concretely:

- **TLS: `rustls` everywhere.** `reqwest` uses `rustls-tls` (no `native-tls`). Server-side TLS (when we eventually add it) uses `axum-server` + `tokio-rustls`.
- **sqlx on SQLite doesn't need TLS at all** — we use `runtime-tokio` (no `-rustls` suffix), since SQLite is a local file.
- **`image` crate, `rustls`, `ring`/`aws-lc-rs`** — all pure Rust. We avoid `openssl`, `native-tls`, `libsqlite3-sys` (bundled SQLite is default).

### Crate selection

| Concern | Crate / tool | Rationale |
|---------|--------------|-----------|
| HTTP framework | `axum` 0.7+ | Ergonomic, tower-based, from the Tokio team |
| Async runtime | `tokio` 1.x | Standard choice; full feature set |
| HTTP client | `reqwest` with `rustls-tls` | Talks to ComfyUI's HTTP API (plain HTTP on localhost; rustls reserved for future HTTPS calls) |
| Database | `sqlx` with SQLite, `runtime-tokio` | Compile-time checked queries, async; no TLS needed for local SQLite |
| TLS (server, later) | `axum-server` + `tokio-rustls` | Terminates HTTPS for Tailscale cert |
| Serialization | `serde` + `serde_json` + `serde_yaml` | Standard |
| Image processing | `image` crate | Thumbnail generation (pure Rust) |
| Logging | `tracing` + `tracing-subscriber` | Structured logs, integrates with axum |
| Middleware | `tower-http` | CORS, limits, tracing, auth helpers |
| Config | plain `toml` + env | Small binary, no overkill config system |
| UUID generation | `uuid` with `v4` feature | Job IDs |
| Testing | built-in `cargo test` | Integration tests via `tower::ServiceExt::oneshot` |

### Cargo.toml dependencies (reference)

```toml
[package]
name = "zun-rust-server"
version = "0.1.0"
edition = "2024"

[dependencies]
tokio = { version = "1", features = ["full"] }
axum = { version = "0.7", features = ["multipart", "macros"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["trace", "limit", "cors"] }
hyper = "1"
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls", "multipart"] }
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "migrate", "chrono"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
image = "0.25"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
thiserror = "1"
anyhow = "1"
bytes = "1"
tokio-util = { version = "0.7", features = ["io"] }
toml = "0.8"

[dev-dependencies]
tower = { version = "0.5", features = ["util"] }
reqwest = { version = "0.12", features = ["multipart"] }
```

Version numbers are a snapshot — bump to current stable at project start.

### Rust edition and toolchain

- Edition 2024 (stable; already set in Cargo.toml)
- MSRV: latest stable at project start; no need to pin an older version for a personal project
- Formatter: `rustfmt` with default settings
- Linter: `cargo clippy` on every commit; treat `clippy::pedantic` as advisory, not blocking

---

## Security model

### Threat model

- Single-user personal service behind Tailscale
- Primary threat: a device on the tailnet other than the developer's phone attempts to hit the API
- Secondary threat: malformed uploads causing server crashes or disk exhaustion

### Layers

1. **Tailscale network boundary.** Server is unreachable from the public internet. Only devices explicitly added to the tailnet can route packets to it.
2. **Bind to tailnet interface only.** `axum` listens on the Tailscale IP (e.g., `100.x.x.x:8443`), not `0.0.0.0`. Even if a firewall rule goes wrong, the service isn't exposed on LAN.
3. **TLS via `tailscale cert` (belt-and-suspenders, optional).** Tailscale's WireGuard tunnel already encrypts every packet end-to-end between your phone and the server, so plain HTTP over the tailnet is not cleartext over the wire. TLS on top still buys you: (a) the browser/Android app doesn't have to special-case an http:// URL, (b) defense-in-depth if you ever route non-tailnet traffic here by accident. For v1 dev we run plain HTTP on `127.0.0.1` and defer TLS to milestone 7.
4. **Bearer token middleware.** Every request (except `/api/health`) requires `Authorization: Bearer <token>`. Token is a long random string (32+ bytes hex).
5. **Upload size and type limits.** Multipart uploads capped at 20 MB, content-type validated as `image/*`, decoded with `image` crate before acceptance (rejects corrupt/malicious payloads).

### Token handling

- **Token lives in an env var (`ZUN_TOKEN`), not in a TOML file.** The systemd unit loads it from a separate, mode-600 EnvironmentFile.
- **Token is compared in constant time** (`subtle::ConstantTimeEq`) to prevent timing side channels, even though realistic attacks are implausible here.
- **Never log the token.** Tracing middleware must redact or omit the `Authorization` header.

### Systemd hardening

The systemd unit for `zun-server.service` should include basic sandboxing:

```ini
[Service]
User=zun
Group=zun
EnvironmentFile=/etc/zun/env        # mode 600, contains ZUN_TOKEN
WorkingDirectory=/srv/zun
ExecStart=/usr/local/bin/zun-server
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/srv/zun
```

The `zun` user should own `/srv/zun/` and nothing else. ComfyUI (from project-zun, run under the developer's user via `just serve`) communicates with this service only via HTTP on `127.0.0.1:8188`.

### What NOT to do

- Do not implement custom auth schemes — just bearer token.
- Do not expose ComfyUI's HTTP API through this server beyond the narrow subset needed.
- Do not serve static files from `/srv/zun/outputs/` via `ServeDir` without auth. Always route through an authenticated handler.
- Do not log request bodies containing image data.

---

## Data model

### Jobs table

```sql
CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,            -- UUID v4
    status          TEXT NOT NULL,               -- queued | running | done | failed
    prompt_id       TEXT NOT NULL,               -- matches prompts.yaml
    input_path      TEXT NOT NULL,               -- relative to /srv/zun
    output_path     TEXT,                        -- relative to /srv/zun
    thumb_path      TEXT,                        -- relative to /srv/zun
    comfy_prompt_id TEXT,                        -- ComfyUI's internal prompt UUID
    error_message   TEXT,                        -- populated when status=failed
    created_at      INTEGER NOT NULL,            -- unix seconds
    started_at      INTEGER,                     -- unix seconds; set when worker picks it up
    completed_at    INTEGER,                     -- unix seconds; set on done/failed
    width           INTEGER,                     -- output dimensions
    height          INTEGER
) STRICT;

CREATE INDEX idx_jobs_status     ON jobs(status);
CREATE INDEX idx_jobs_created    ON jobs(created_at DESC);
CREATE INDEX idx_jobs_status_created ON jobs(status, created_at DESC);
```

The `STRICT` table option enforces type declarations (SQLite 3.37+). Strongly recommended.

### Migrations

Use `sqlx-cli` for migrations:

```
migrations/
  20260101000000_init.sql      -- creates jobs table, indexes, sets PRAGMAs
```

Apply via `sqlx::migrate!().run(&pool).await` at startup.

### Startup PRAGMAs

Run these after opening the pool for reliability and concurrency:

```rust
sqlx::query("PRAGMA journal_mode = WAL").execute(&pool).await?;
sqlx::query("PRAGMA synchronous = NORMAL").execute(&pool).await?;
sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await?;
sqlx::query("PRAGMA busy_timeout = 5000").execute(&pool).await?;
```

WAL mode allows readers and writers to operate concurrently. `synchronous = NORMAL` is safe with WAL and dramatically improves write throughput.

### Backup strategy

Not critical for v1 (the images are regeneratable, and the DB is small). If backups become desired:

```bash
sqlite3 /srv/zun/jobs.db ".backup /srv/zun/backups/jobs-$(date +%F).db"
```

Plus rsync of `/srv/zun/inputs/` and `/srv/zun/outputs/` to another machine. Weekly cron is fine.

---

## API contract

All endpoints live under `/api/`. All require `Authorization: Bearer <token>` except `/api/health`. Content-Type is `application/json` unless otherwise noted.

### Endpoints

#### `GET /api/health`

Unauthenticated. Lightweight liveness check.

Response 200:
```json
{ "status": "ok", "version": "0.1.0" }
```

#### `GET /api/prompts`

List predefined prompts.

Response 200:
```json
[
  { "id": "anime_style", "label": "Anime-ify", "description": "Transform into anime style" },
  { "id": "oil_painting", "label": "Oil painting", "description": "Classic oil painting rendering" }
]
```

#### `POST /api/jobs`

Submit a new job. `multipart/form-data`:
- `image` (file, required): input image (JPEG or PNG, up to 20 MB)
- `prompt_id` (field, required): must match an id in `prompts.yaml`

Response 201:
```json
{ "job_id": "a7f2e1b9-..." }
```

Response 400: bad input (too large, missing fields, unsupported content type, unknown prompt_id).

#### `GET /api/jobs/{id}`

Get status of a specific job.

Response 200:
```json
{
  "id": "a7f2e1b9-...",
  "status": "running",
  "prompt_id": "oil_painting",
  "prompt_label": "Oil painting",
  "progress": 0.48,
  "error": null,
  "created_at": 1729555200,
  "completed_at": null,
  "width": null,
  "height": null
}
```

`progress` is optional; populated when ComfyUI reports it. `error` is populated when `status = failed`.

Response 404 if job does not exist.

#### `GET /api/jobs`

Paginated list of jobs.

Query params:
- `status` (default `done`): filter by status
- `limit` (default 30, max 100): page size
- `before` (optional, unix seconds): return jobs created strictly before this timestamp

Response 200:
```json
[
  {
    "id": "a7f2e1b9-...",
    "prompt_id": "oil_painting",
    "prompt_label": "Oil painting",
    "created_at": 1729555200,
    "duration_seconds": 24
  }
]
```

Pagination: client passes `before=<earliest_created_at_from_previous_page>` to fetch next page. No page cursor tokens — timestamps are sufficient.

#### `GET /api/jobs/{id}/thumb`

Serve ~400px thumbnail of the output. Generated on demand if missing.

Response 200: `image/jpeg` bytes.

#### `GET /api/jobs/{id}/input`

Serve the original uploaded input image.

Response 200: `image/jpeg` or `image/png` bytes.

#### `GET /api/jobs/{id}/result`

Serve the full-resolution output.

Response 200: image bytes.
Response 409 if job is not yet `done`.

#### `DELETE /api/jobs/{id}`

Delete a job: removes DB row and all associated files (input, output, thumb).

Response 204 on success. Response 404 if not found.

### Error response shape

All errors return JSON:

```json
{ "error": "human-readable message", "code": "invalid_prompt_id" }
```

### Content negotiation

- JSON request and response bodies only (except image endpoints).
- Image endpoints set `Content-Type` based on file extension (jpeg, png).
- `Cache-Control: private, max-age=3600` on thumbnail and result endpoints since contents are immutable once generated.

---

## ComfyUI integration

### Communication pattern

The Rust server talks to ComfyUI over HTTP on localhost. ComfyUI exposes a small set of endpoints:

| ComfyUI endpoint | Purpose |
|------------------|---------|
| `POST /prompt` | Submit a workflow; returns `{prompt_id}` |
| `GET /history/{prompt_id}` | Get execution status and outputs |
| `GET /view?filename=...&type=output` | Download a generated image |
| `GET /system_stats` | Optional: check ComfyUI is alive |

### Workflow templates: registry, not singleton

The Python side in `project-zun/workflows/` ships multiple templates, each exported from ComfyUI's UI via "Save (API format)". For v1 the server cares about:

| Workflow file | Family | Use |
|---------------|--------|-----|
| `flux2_klein_edit.json` | FLUX 2 klein | **Primary v1 target** — fast maskless edit |
| `flux_fill_auto_mask.json` | FLUX.1 Fill | Secondary; mask-based inpainting |
| (future) | any | Add by dropping a JSON + adding a prompts.yaml entry |

The server:
- On startup, scans its configured `workflows/` directory and loads every `*.json` into `HashMap<String, serde_json::Value>` keyed by filename (without `.json`).
- Does **not** hardcode node IDs. It treats each template as opaque — substitution happens via string placeholders (see below).
- Each prompt in `prompts.yaml` declares which workflow file it binds to.

### Placeholder substitution (not node-ID patching)

project-zun's `scripts/run_workflow.py` already uses a **string-placeholder** pattern that works across every workflow without the Rust side knowing node IDs. The workflow JSONs contain literal tokens like `PROMPT_PLACEHOLDER` and (future) `LORA_PLACEHOLDER` at the right slots. We do a JSON-aware walk and swap them.

```rust
/// Walk the workflow JSON, replacing every "PROMPT_PLACEHOLDER" string with
/// the prompt text. Returns a patched clone.
pub fn build_workflow(
    template: &serde_json::Value,
    prompt_text: &str,
    input_image_name: &str,
    job_id: &str,
) -> serde_json::Value {
    let mut workflow = template.clone();
    patch_placeholders(&mut workflow, &[
        ("PROMPT_PLACEHOLDER", prompt_text),
        ("INPUT_IMAGE_PLACEHOLDER", input_image_name),
        ("FILENAME_PREFIX_PLACEHOLDER", &format!("zun_{job_id}")),
    ]);
    workflow
}

fn patch_placeholders(value: &mut serde_json::Value, subs: &[(&str, &str)]) {
    match value {
        serde_json::Value::String(s) => {
            for (needle, replacement) in subs {
                if s == needle {
                    *s = replacement.to_string();
                }
            }
        }
        serde_json::Value::Array(arr) => arr.iter_mut().for_each(|v| patch_placeholders(v, subs)),
        serde_json::Value::Object(obj) => obj.values_mut().for_each(|v| patch_placeholders(v, subs)),
        _ => {}
    }
}
```

This keeps the Rust code workflow-agnostic and consistent with `scripts/run_workflow.py`. If project-zun's workflows don't already use these exact placeholder names, we'll align on a set (`PROMPT_PLACEHOLDER` is already there — `INPUT_IMAGE_PLACEHOLDER` and `FILENAME_PREFIX_PLACEHOLDER` we'd add to the JSONs as a one-time edit).

### Passing images to ComfyUI: `/upload/image`

Upload each input via ComfyUI's `POST /upload/image` endpoint rather than relying on shared filesystem / symlinks.

Why:
- **No filesystem coupling.** The Rust server's data dir and ComfyUI's `input/` dir can live anywhere (project-zun keeps ComfyUI under `project-zun/ComfyUI/input/`). No symlink to maintain; survives moving either project.
- **Same host today, could be different host tomorrow.** Option A locks us to co-located processes.
- **One extra HTTP round-trip per job.** Negligible (~10 ms for a 1 MB JPEG on loopback) vs. a ~30 s generation.

```rust
async fn upload_image(
    client: &reqwest::Client,
    comfy_base: &str,
    bytes: Vec<u8>,
    filename: &str,  // e.g., "zun_{job_id}.jpg"
) -> Result<String, AppError> {
    let form = reqwest::multipart::Form::new()
        .part("image", reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string()))
        .text("type", "input")
        .text("overwrite", "true");
    let resp: serde_json::Value = client
        .post(format!("{comfy_base}/upload/image"))
        .multipart(form)
        .send().await?
        .error_for_status()?
        .json().await?;
    resp["name"].as_str().map(String::from).ok_or(AppError::ComfyBadResponse)
}
```

The returned `name` is what the workflow's `LoadImage` node needs to reference — it becomes our `INPUT_IMAGE_PLACEHOLDER` substitution.

**Caveat:** ComfyUI has no "delete uploaded image" endpoint. Inputs accumulate in its `input/` dir over time. For a personal service this is fine; a periodic `find .../ComfyUI/input -mtime +7 -delete` cron is the simplest cleanup.

### Output retrieval: `/view` or shared `--output-directory`?

ComfyUI's `/history/{prompt_id}` returns output filenames in its `output/` directory. Two ways to get the bytes back:

- **Download via `GET /view?filename=...&type=output`.** Zero coupling. Outputs get double-stored briefly (ComfyUI's dir + our `outputs/`) until we delete from ComfyUI's — and again, no delete endpoint.
- **Configure ComfyUI to write directly to our data dir** via `--output-directory /path/to/zun-rust-server/data/outputs`. Single copy, no extra HTTP call.

**Recommended: `/view` for v1.** It's more portable and the coupling is one-directional (we depend on ComfyUI, not the other way around). Switch to the shared-output-dir approach if the double-storage becomes a real problem.

### Sidecar JSON metadata

project-zun writes a `{basename}.json` sidecar next to every output PNG, recording `{kind, prompt, seed, steps, workflow, lora, timestamp}`. The Python tool `scripts/contact_sheet.py` reads these to build galleries.

For v1, the server emits the same sidecar format on completion, so both sides can interoperate. SQLite is still the primary source of truth for the Android app; sidecar JSON is a cheap cross-tool convenience.

### Submitting a job

```rust
async fn submit_to_comfy(
    client: &reqwest::Client,
    comfy_base: &str,
    workflow: serde_json::Value,
) -> Result<String, AppError> {
    let resp = client
        .post(format!("{}/prompt", comfy_base))
        .json(&serde_json::json!({ "prompt": workflow }))
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = resp.json().await?;
    body["prompt_id"]
        .as_str()
        .map(String::from)
        .ok_or(AppError::ComfyBadResponse)
}
```

### Polling ComfyUI for completion

```rust
async fn poll_comfy(
    client: &reqwest::Client,
    comfy_base: &str,
    prompt_id: &str,
) -> Result<ComfyResult, AppError> {
    loop {
        let resp: serde_json::Value = client
            .get(format!("{}/history/{}", comfy_base, prompt_id))
            .send()
            .await?
            .json()
            .await?;

        if let Some(entry) = resp.get(prompt_id) {
            // Workflow executed; parse outputs
            return Ok(parse_comfy_result(entry)?);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
```

The response shape has `outputs` keyed by the `SaveImage` node ID. Each entry lists `images` with `filename`, `subfolder`, and `type` ("output"). The worker downloads these via `/view`.

### Websocket option (deferred)

ComfyUI also exposes a websocket that emits real-time `progress` and `executing` events. Useful for populating the `progress` field on job status. For v1, HTTP polling is sufficient; switch to websockets if you want smooth progress bars in the app.

### Error paths

ComfyUI may return:
- HTTP 400 on invalid workflow JSON → job fails with descriptive error
- HTTP 500 during execution (OOM, bad sampler, etc.) → job fails
- Hanging (no response in `/history` for too long) → timeout after N minutes, mark job as failed

Set a reasonable overall timeout per job (e.g., 5 minutes). Use `tokio::time::timeout`.

---

## Job lifecycle and worker

### States

```
queued --> running --> done
             |
             +--> failed
```

- `queued` — row exists, input file saved, waiting for worker
- `running` — worker has picked it up, ComfyUI is processing
- `done` — output file exists, thumbnail generated, `output_path` populated
- `failed` — ComfyUI or server error; `error_message` populated

### Worker loop

Single background tokio task, spawned at startup:

```rust
async fn worker_loop(state: AppState, mut wake: mpsc::Receiver<()>) {
    loop {
        // Reset any orphaned 'running' rows from previous crash
        sqlx::query("UPDATE jobs SET status = 'queued' WHERE status = 'running'")
            .execute(&state.db).await.ok();

        // Drain queued jobs in FIFO order
        while let Some(job) = fetch_oldest_queued(&state.db).await.ok().flatten() {
            if let Err(e) = process_job(&state, &job).await {
                mark_failed(&state.db, &job.id, &e.to_string()).await.ok();
                tracing::error!(job_id = %job.id, error = %e, "job failed");
            }
        }

        // Sleep until woken by a new submission (or timeout fallback)
        tokio::select! {
            _ = wake.recv() => {},
            _ = tokio::time::sleep(Duration::from_secs(30)) => {},
        }
    }
}

async fn process_job(state: &AppState, job: &Job) -> Result<(), AppError> {
    mark_running(&state.db, &job.id).await?;

    let prompt = state.prompts.get(&job.prompt_id)
        .ok_or(AppError::UnknownPrompt)?;
    let template = state.workflows.get(&prompt.workflow)
        .ok_or(AppError::WorkflowNotFound)?;

    let uploaded_name = upload_image(&state.http, &state.config.comfy_url,
        std::fs::read(&job.input_path)?, &format!("zun_{}.jpg", job.id)).await?;
    let workflow = build_workflow(template, &prompt.text, &uploaded_name, &job.id);
    let comfy_prompt_id = submit_to_comfy(&state.http, &state.config.comfy_url, workflow).await?;
    update_comfy_id(&state.db, &job.id, &comfy_prompt_id).await?;

    let result = tokio::time::timeout(
        Duration::from_secs(300),
        poll_comfy(&state.http, &state.config.comfy_url, &comfy_prompt_id),
    ).await.map_err(|_| AppError::ComfyTimeout)??;

    let output_path = download_comfy_output(&state, &result).await?;
    let thumb_path = generate_thumbnail(&output_path)?;

    mark_done(&state.db, &job.id, &output_path, &thumb_path, result.width, result.height).await?;
    Ok(())
}
```

### Crash recovery

- On startup, reset any `running` jobs to `queued` (they'll be retried).
- If this proves problematic (e.g., a job crashed the server and retrying loops), add a `retry_count` column and mark as `failed` after 3 retries.

### Concurrency

Process one job at a time. FLUX2 saturates the GPU; parallelism would just thrash VRAM. The `while let Some(job) = fetch_oldest_queued(...)` loop is serial by construction.

### Waking the worker

When `POST /api/jobs` inserts a queued row, it sends `()` on the `wake` channel to notify the worker immediately. If the channel is full (unbuffered/bounded), drop the send — the worker will catch up on its 30s timer.

```rust
let _ = state.worker_tx.try_send(());
```

---

## File and storage layout

### Directory structure

```
/srv/zun/
  jobs.db                  # SQLite main database
  jobs.db-wal              # WAL file (auto-managed)
  jobs.db-shm              # shared memory (auto-managed)
  inputs/
    {uuid}.jpg             # uploaded inputs, one per job
  outputs/
    {uuid}.png             # ComfyUI results
  thumbs/
    {uuid}.jpg             # 400px JPEG thumbnails
  workflows/
    flux2_klein_edit.json   # primary v1 workflow (FLUX 2 klein)
    flux_fill_auto_mask.json  # optional secondary (FLUX.1 Fill)
  prompts.yaml             # prompt catalog
  config.toml              # non-secret runtime config
```

### Path conventions

- Input: `inputs/{job_id}.{ext}` where ext matches the uploaded content-type
- Output: `outputs/{job_id}.png` (ComfyUI outputs PNG by default)
- Thumb: `thumbs/{job_id}.jpg` (always JPEG for size)

Store paths in the DB as relative to `/srv/zun` so the data dir is relocatable.

### Ownership and permissions

- Directory owner: `zun:zun`, mode 755
- Files: mode 644
- ComfyUI user needs read access to `inputs/` and write access to `outputs/`. If ComfyUI runs as a different user, put both in a shared group (e.g., `fluximg`) and set directory modes to 2775 (setgid).

### Cleanup policy

Implement an optional nightly cleanup task that deletes jobs older than N days:

```rust
async fn cleanup_old_jobs(state: &AppState, retention_days: i64) -> Result<(), AppError> {
    let cutoff = chrono::Utc::now().timestamp() - retention_days * 86400;
    let old = sqlx::query_as!(Job,
        "SELECT * FROM jobs WHERE created_at < ?",
        cutoff
    ).fetch_all(&state.db).await?;

    for job in old {
        delete_job_files(&state.config.data_dir, &job).await.ok();
        sqlx::query!("DELETE FROM jobs WHERE id = ?", job.id)
            .execute(&state.db).await.ok();
    }
    Ok(())
}
```

Scheduled with `tokio::time::interval`. Default: disabled. Enable via config when storage becomes a concern.

### Disk usage expectations

- Input JPEGs after Android-side downscaling: 300 KB – 1 MB each
- Output PNGs from FLUX2 at 1024×1024: 1–3 MB each
- Thumbnails: 20–50 KB each
- SQLite DB: tens of KB even with thousands of rows

Total per job: ~2–4 MB. A 100 GB partition holds tens of thousands of jobs comfortably.

---

## Thumbnail generation

Thumbnails are generated synchronously when a job completes. They speed up the gallery grid dramatically — loading dozens of 2 MB PNGs over a mobile network is painful; 30 KB thumbs are instant.

### Implementation

```rust
use image::{imageops::FilterType, ImageFormat};

fn generate_thumbnail(output_path: &Path, thumb_path: &Path) -> Result<(), AppError> {
    let img = image::open(output_path)?;
    let thumb = img.resize(400, 400, FilterType::Lanczos3);
    thumb.save_with_format(thumb_path, ImageFormat::Jpeg)?;
    Ok(())
}
```

- `resize` preserves aspect ratio and fits inside 400×400
- Lanczos3 gives good quality; Triangle is faster if CPU matters
- JPEG quality defaults to 75; adjust via `image::codecs::jpeg::JpegEncoder` if needed

### Lazy backfill

If a thumbnail is missing when requested (e.g., after migrating from an older version), generate it on demand in the handler:

```rust
async fn serve_thumb(job_id: &str, state: &AppState) -> Result<Response, AppError> {
    let thumb_path = thumb_path_for(&state.config.data_dir, job_id);
    if !thumb_path.exists() {
        let job = get_job(&state.db, job_id).await?;
        let output = data_path(&state.config.data_dir, &job.output_path.ok_or(AppError::NotReady)?);
        tokio::task::spawn_blocking(move || generate_thumbnail(&output, &thumb_path)).await??;
    }
    Ok(ServeFile::new(thumb_path).try_into_response().await?)
}
```

`spawn_blocking` prevents blocking the tokio runtime during CPU-bound resizing.

---

## Prompts configuration

`prompts.yaml` defines the catalog of predefined prompts. Edit-and-restart workflow: change the file, `systemctl restart zun-server`. The Android app fetches `/api/prompts` on home screen entry.

### Format

```yaml
# Each prompt binds to a workflow JSON in workflows/ by stem.
# v1 supports flux2_klein_edit (primary) and flux_fill_auto_mask (secondary).
# Add new workflows by dropping the JSON and referencing it here — no code change.

prompts:
  - id: anime_style
    label: Anime-ify
    description: Transform into anime style with cel shading
    text: >-
      transform into anime style, cel shaded, vibrant colors,
      clean line art, studio ghibli aesthetic
    workflow: flux2_klein_edit

  - id: oil_painting
    label: Oil painting
    description: Classic oil painting rendering
    text: >-
      oil painting, visible brush strokes, rich impasto texture,
      Rembrandt lighting, canvas texture
    workflow: flux2_klein_edit

  - id: remove_bg
    label: Remove background
    description: Isolate subject on pure white background via mask-based inpaint
    text: >-
      isolated subject on pure white background, clean edges, studio photography
    workflow: flux_fill_auto_mask
```

`workflow` is the JSON filename without `.json`. `negative` prompts are omitted in v1 — FLUX 2 klein doesn't use them; FLUX.1 Fill workflows embed a fixed negative. We'll add the field back (as optional) if a workflow ever needs it.

### Rust type

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Prompt {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub text: String,
    pub workflow: String,  // workflow filename stem, e.g., "flux2_klein_edit"
}

#[derive(serde::Deserialize)]
struct PromptsFile {
    prompts: Vec<Prompt>,
}

pub fn load_prompts(path: &Path) -> Result<HashMap<String, Prompt>, AppError> {
    let raw = std::fs::read_to_string(path)?;
    let parsed: PromptsFile = serde_yaml::from_str(&raw)?;
    Ok(parsed.prompts.into_iter().map(|p| (p.id.clone(), p)).collect())
}
```

### Public vs. internal fields

`GET /api/prompts` returns only `id`, `label`, `description`. The actual prompt text and negative prompt are internal — the Android app doesn't need to see them. This keeps prompt engineering details on the server.

```rust
#[derive(serde::Serialize)]
struct PromptDto {
    id: String,
    label: String,
    description: Option<String>,
}

impl From<&Prompt> for PromptDto {
    fn from(p: &Prompt) -> Self {
        PromptDto { id: p.id.clone(), label: p.label.clone(), description: p.description.clone() }
    }
}
```

---

## Error handling

### Unified error type

Use `thiserror` for a single `AppError` that implements `axum::response::IntoResponse`. This maps internal errors to HTTP responses uniformly.

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unknown prompt id: {0}")]
    UnknownPrompt(String),

    #[error("job not ready yet")]
    NotReady,

    #[error("comfyui error: {0}")]
    Comfy(String),

    #[error("comfyui timed out")]
    ComfyTimeout,

    #[error("workflow template missing")]
    WorkflowNotFound,

    #[error("comfyui returned bad response")]
    ComfyBadResponse,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Db(#[from] sqlx::Error),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Image(#[from] image::ImageError),

    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::UnknownPrompt(_) => (StatusCode::BAD_REQUEST, "invalid_prompt_id"),
            AppError::NotReady => (StatusCode::CONFLICT, "not_ready"),
            AppError::Comfy(_) | AppError::ComfyTimeout
                | AppError::ComfyBadResponse | AppError::WorkflowNotFound
                | AppError::Io(_) | AppError::Db(_) | AppError::Http(_)
                | AppError::Json(_) | AppError::Image(_) | AppError::Internal(_) =>
                (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };

        // Log 5xx errors with details; log 4xx at info level
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::info!(error = %self, "client error");
        }

        let body = Json(json!({
            "error": self.to_string(),
            "code": code,
        }));
        (status, body).into_response()
    }
}
```

### When to propagate vs. handle

- **Handler functions return `Result<T, AppError>`.** axum serializes the error via `IntoResponse`.
- **Worker tasks log and continue.** A failed job marks the row as `failed` with the error message; the worker moves on.
- **Startup errors panic.** If the DB can't open or workflows can't load, the service can't usefully run. Let systemd restart it.

### Logging conventions

Use `tracing` with structured fields:

```rust
tracing::info!(job_id = %id, status = "queued", "job submitted");
tracing::error!(job_id = %id, error = %e, "job failed");
```

Log levels:
- `error`: 5xx, worker failures, panics
- `warn`: unexpected but recoverable (e.g., slow ComfyUI response)
- `info`: job lifecycle events, request summaries (handled by `tower-http` tracing)
- `debug`: detailed internals, disabled in production
- `trace`: verbose (e.g., raw ComfyUI responses)

Configure via `RUST_LOG` env var: `RUST_LOG=zun_rust_server=info,tower_http=info`.

---

## Project structure

```
zun-rust-server/
├── Cargo.toml
├── Cargo.lock
├── .gitignore                  # includes target/, *.db, *.db-*, /etc env files
├── migrations/
│   └── 20260101000000_init.sql
├── workflows/
│   └── flux2_klein_edit.json   # primary; copied in from project-zun/workflows/
├── config.example.toml         # template config
├── prompts.example.yaml        # template prompts
├── deploy/
│   ├── zun-server.service         # systemd unit
│   └── env.example             # template for EnvironmentFile
├── src/
│   ├── main.rs                 # startup, runtime, server bind
│   ├── config.rs               # Config struct, loading
│   ├── state.rs                # AppState, shared context
│   ├── error.rs                # AppError, IntoResponse
│   ├── auth.rs                 # bearer token middleware
│   ├── db.rs                   # pool setup, migrations, typed queries
│   ├── models.rs               # Job, Prompt, domain types
│   ├── prompts.rs              # YAML loader
│   ├── handlers/
│   │   ├── mod.rs
│   │   ├── health.rs
│   │   ├── prompts.rs
│   │   ├── jobs_submit.rs      # POST /api/jobs
│   │   ├── jobs_status.rs      # GET /api/jobs/{id}
│   │   ├── jobs_list.rs        # GET /api/jobs
│   │   ├── jobs_delete.rs      # DELETE /api/jobs/{id}
│   │   └── jobs_files.rs       # input/thumb/result serving
│   ├── comfy/
│   │   ├── mod.rs
│   │   ├── client.rs           # HTTP client wrapper
│   │   └── workflow.rs         # template loading, substitution
│   ├── worker.rs               # background job processor
│   ├── thumb.rs                # thumbnail generation
│   └── util.rs                 # misc helpers
├── tests/
│   ├── common/
│   │   ├── mod.rs              # test harness: spin up AppState with temp DB
│   │   └── mock_comfy.rs       # wiremock-based ComfyUI fake
│   ├── health.rs
│   ├── jobs.rs                 # submission, polling, listing
│   └── auth.rs                 # bearer token enforcement
└── scripts/
    ├── test.sh                 # curl-based end-to-end smoke test
    └── seed_jobs.sh            # populate DB with sample jobs for UI testing
```

### Minimal file count philosophy

- Handlers split by route family, not by HTTP verb.
- Model types live in `models.rs` (Job, Prompt) — no need for separate `domain/` layer.
- Migration files per schema change; don't edit once applied.

### `.gitignore` essentials

```
/target/
*.db
*.db-shm
*.db-wal
/etc/env
deploy/env
config.toml
prompts.yaml
workflows/*.json
!workflows/*.example.json
```

Workflow templates live in project-zun (the source of truth). The server reads them from a configured `workflows/` directory — by default symlinked/copied from `../project-zun/workflows/` in dev. No workflow JSONs are committed to this repo.

---

## Build order and milestones

Build strictly in this order. Each milestone is independently testable with `curl`.

### Milestone 1 — Hello world axum server ✓

- [x] `cargo new --bin zun-rust-server`
- [x] Add minimal dependencies: `axum`, `tokio`, `tracing`, `tracing-subscriber`, `serde_json`
- [x] Library + binary split: `src/lib.rs` exposes `router()` and `VERSION`; `src/main.rs` is the binary entry
- [x] `GET /api/health` → `{"status":"ok","version":"0.1.0"}`
- [x] Listen on `127.0.0.1:8080`
- [x] Set up `tracing-subscriber` with env filter (default `zun_rust_server=info,tower_http=info`)
- [x] Integration tests (`tests/health.rs`): asserts 200 + body shape; asserts 404 on unknown route
- [x] Pre-commit hook at `.git/hooks/pre-commit`: gates commits on `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings`

**Done.** All tests pass; both commit gates clean on the tree.

### Milestone 2 — Config, state, and SQLite ✓

- [x] Add `sqlx` (no default features; `runtime-tokio,sqlite,macros,migrate,chrono`), `serde`, `uuid`, `chrono`, `anyhow`; `tempfile` as dev-dep. TOML config crate deferred — env is enough for now.
- [x] `Config` struct loaded from env vars: `ZUN_DATA_DIR` (default `./data`), `ZUN_BIND` (default `127.0.0.1:8080`)
- [x] `AppState { db: SqlitePool, config: Config }` cloneable, passed via `Router::with_state`
- [x] `migrations/20260422000000_init.sql` with `jobs` table + three indexes (STRICT)
- [x] `db::init()` opens pool with `SqliteConnectOptions` (WAL, NORMAL sync, foreign_keys, busy_timeout=5s) and runs migrations at startup
- [x] Runtime `sqlx::query`/`sqlx::query_as` (not macros) — no build-time DB metadata needed while schema is in flux. Upgrade to compile-time macros in M5+.
- [x] Debug endpoints: `POST /api/debug/job` returns `{id}`; `GET /api/debug/jobs` returns a list of `{id, status, prompt_id, created_at}` newest-first
- [x] Integration tests (`tests/jobs.rs`, `tests/common/mod.rs`): each test gets a fresh temp-dir SQLite via `test_app()`. 3 scenarios covered (empty list, insert+list, ordering). All 5 tests across the suite pass.

**Done.** Two new source modules (`config.rs`, `db.rs`, `state.rs`, `handlers.rs`), one migration. `config.toml` parsing will return when we need a non-env setting.

### Milestone 3 — Bearer auth middleware

- [ ] Add `tower` and `tower-http`
- [ ] Write auth middleware: check `Authorization: Bearer <token>` against `config.token`
- [ ] Apply to all routes except `/api/health`
- [ ] Remove debug endpoints from previous milestone (or keep under auth)
- [ ] Verify: curl without token returns 401; with token returns 200

**Done when:** auth works; no route leaks without the token.

### Milestone 4 — Upload endpoint

- [ ] Enable `axum` `multipart` feature
- [ ] Add `tower-http` body limit (20 MB)
- [ ] Implement `POST /api/jobs`: parse multipart, validate `prompt_id`, write file to `inputs/`, insert row, return `{job_id}`
- [ ] Load prompts from `prompts.yaml` at startup
- [ ] Add `GET /api/prompts`
- [ ] Add `GET /api/jobs/{id}` returning status
- [ ] Verify with curl: submit an image, fetch status (should be `queued`)

**Done when:** full submit + status flow works without ComfyUI integration (status stays queued).

### Milestone 5 — ComfyUI integration

- [ ] Export your working FLUX2 workflow as API JSON, commit to `workflows/`
- [ ] Write `comfy::client` with `submit_prompt` and `poll_history` functions
- [ ] Write `comfy::workflow::build_workflow` with node-ID substitution
- [ ] Implement the worker loop in `worker.rs`
- [ ] Spawn worker in `main.rs` after server setup
- [ ] On job submission, send `()` on wake channel
- [ ] Download ComfyUI outputs to `/srv/zun/outputs/`
- [ ] Update DB with output_path, status=done, timestamps
- [ ] Verify end-to-end: submit image, poll status, fetch result

**Done when:** `curl -F image=@photo.jpg -F prompt_id=anime_style http://localhost:8080/api/jobs` returns a job_id, and after ~30s the status becomes `done` with a real image at `/api/jobs/{id}/result`.

### Milestone 6 — Gallery endpoints

- [ ] Implement thumbnail generation in `thumb.rs`
- [ ] Generate thumb on job completion (inside worker)
- [ ] Implement `GET /api/jobs?status=done&limit=30&before=<ts>` with pagination
- [ ] Implement `GET /api/jobs/{id}/input` and `/thumb` handlers
- [ ] Implement `DELETE /api/jobs/{id}` (remove row and all files)
- [ ] Backfill script: generate thumbnails for existing jobs

**Done when:** can list 30 past jobs, fetch their thumbnails, delete them.

### Milestone 7 — Tailscale and TLS

- [ ] Install Tailscale on server: `tailscale up`
- [ ] Generate cert: `tailscale cert your-server.your-tailnet.ts.net`
- [ ] Configure `axum` with `axum-server` + rustls to serve HTTPS
- [ ] Bind to the Tailscale IP (`100.x.x.x`) not `0.0.0.0`
- [ ] Verify from another device on the tailnet: `curl https://your-server.your-tailnet.ts.net/api/health`

**Done when:** HTTPS works from your phone over Tailscale.

### Milestone 8 — Systemd deployment

- [ ] Write `deploy/zun-server.service` unit
- [ ] Create `zun` system user, `/srv/zun/` directory
- [ ] Create `/etc/zun/env` with `ZUN_TOKEN=...`, mode 600
- [ ] `cargo build --release`, copy binary to `/usr/local/bin/zun-server`
- [ ] `systemctl enable --now zun-server`
- [ ] Verify logs: `journalctl -u zun-server -f`

**Done when:** service starts on boot, survives reboots, logs flow correctly.

### Milestone 9 — Polish

- [ ] Per-job timeout (5 minutes default)
- [ ] Startup reset of orphaned `running` jobs
- [ ] Graceful shutdown (finish current job, don't accept new requests)
- [ ] Cleanup task for old jobs (opt-in via config)
- [ ] Metrics/health endpoints (e.g., `/api/health` returns queue depth)

**Done when:** server feels production-solid for personal use.

---

## Testing approach

### Level 1 — curl scripts

Maintain `scripts/test.sh` as the primary end-to-end smoke test. Run after every significant change.

```bash
#!/usr/bin/env bash
set -euo pipefail
BASE="${BASE:-http://localhost:8080}"
TOKEN="${TOKEN:-test-token}"
AUTH="Authorization: Bearer $TOKEN"

echo "--- health"
curl -sf "$BASE/api/health" | jq .

echo "--- prompts"
curl -sf -H "$AUTH" "$BASE/api/prompts" | jq 'length'

echo "--- submit"
JOB_ID=$(curl -sf -H "$AUTH" \
  -F "image=@tests/fixtures/sample.jpg" \
  -F "prompt_id=anime_style" \
  "$BASE/api/jobs" | jq -r .job_id)
echo "job_id=$JOB_ID"

echo "--- poll"
for i in $(seq 1 60); do
  STATUS=$(curl -sf -H "$AUTH" "$BASE/api/jobs/$JOB_ID" | jq -r .status)
  echo "[$i] status=$STATUS"
  case "$STATUS" in
    done) break ;;
    failed) echo "job failed"; exit 1 ;;
  esac
  sleep 2
done

echo "--- fetch result"
curl -sf -H "$AUTH" "$BASE/api/jobs/$JOB_ID/result" -o /tmp/result.png
file /tmp/result.png

echo "--- list jobs"
curl -sf -H "$AUTH" "$BASE/api/jobs?limit=5" | jq 'length'

echo "--- delete"
curl -sf -X DELETE -H "$AUTH" "$BASE/api/jobs/$JOB_ID" -o /dev/null
echo "deleted"
```

### Level 2 — Rust integration tests

In `tests/`, spin up a test AppState with a temp SQLite DB and a wiremock-based fake ComfyUI. Exercise handlers via `tower::ServiceExt::oneshot` so no real TCP is involved.

```rust
// tests/jobs.rs
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
mod common;

#[tokio::test]
async fn submit_and_get_status() {
    let app = common::test_app().await;

    let body = common::multipart_body_with_image("test_prompt");
    let resp = app.clone().oneshot(
        Request::post("/api/jobs")
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", body.content_type())
            .body(body.into_body()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    // ... parse job_id, fetch status, etc.
}
```

Priorities for integration tests:
- `POST /api/jobs` with valid and invalid inputs (missing fields, too large, bad prompt_id)
- Bearer auth enforcement
- Pagination of `GET /api/jobs`
- `DELETE /api/jobs/{id}` removes both DB row and files
- Worker correctly transitions queued → running → done against mocked ComfyUI

### Level 3 — unit tests

Keep minimal. Worthwhile targets:
- `workflow::build_workflow` — verify node-ID substitution with known inputs
- `prompts::load_prompts` — verify YAML parsing of edge cases
- `thumb::generate_thumbnail` — runs on a test image; checks dimensions

### Manual test checklist (after deploy)

- [ ] `systemctl status zun-server` shows active
- [ ] Logs flow via `journalctl -u zun-server -f`
- [ ] curl over Tailscale TLS works
- [ ] Submit real job, watch it complete end-to-end
- [ ] Kill ComfyUI mid-job → job marked failed
- [ ] Kill server mid-job → on restart, `running` reset to `queued`, job resumes
- [ ] Send request without token → 401
- [ ] Send oversized upload → 413 or 400

---

## Deployment

### Target environment

- Ubuntu 24.04 LTS (or similar modern Linux)
- GPU drivers installed and working with ComfyUI
- `tailscaled` running, device added to tailnet
- Systemd for service management
- Ports: 8443 (HTTPS for the API), 8188 (ComfyUI, localhost only)

### Prerequisites

```bash
# Create service user
sudo useradd -r -s /usr/sbin/nologin -d /srv/zun zun

# Create data directory
sudo mkdir -p /srv/zun/{inputs,outputs,thumbs,workflows}
sudo chown -R zun:zun /srv/zun
sudo chmod 755 /srv/zun

# Create config directory
sudo mkdir -p /etc/zun
sudo chown root:zun /etc/zun
sudo chmod 750 /etc/zun
```

### Secrets

`/etc/zun/env` (mode 600, owned by root):

```
ZUN_TOKEN=<64+ character random string>
RUST_LOG=zun_rust_server=info,tower_http=info
```

Generate the token:

```bash
openssl rand -hex 32 | sudo tee /etc/zun/env
# Then edit to prepend ZUN_TOKEN=
sudo chmod 600 /etc/zun/env
sudo chown root:root /etc/zun/env
```

### Config file

`/etc/zun/config.toml` (readable by zun user):

```toml
[server]
bind_address = "100.64.0.5:8443"        # your Tailscale IP
tls_cert = "/etc/zun/tls/fullchain.pem"
tls_key  = "/etc/zun/tls/privkey.pem"

[data]
dir = "/srv/zun"

[comfyui]
url = "http://127.0.0.1:8188"
job_timeout_seconds = 300

[cleanup]
enabled = false
retention_days = 30
```

### TLS via Tailscale

```bash
sudo mkdir -p /etc/zun/tls
cd /etc/zun/tls
sudo tailscale cert your-server.your-tailnet.ts.net
# produces your-server.your-tailnet.ts.net.crt and .key
sudo mv your-server.your-tailnet.ts.net.crt fullchain.pem
sudo mv your-server.your-tailnet.ts.net.key privkey.pem
sudo chown root:zun /etc/zun/tls/*.pem
sudo chmod 640 /etc/zun/tls/*.pem
```

Tailscale certs are valid for 90 days. Renew via a systemd timer or cron job:

```
0 3 * * 0 cd /etc/zun/tls && tailscale cert your-server.your-tailnet.ts.net && systemctl reload zun-server
```

### Systemd unit

`/etc/systemd/system/zun-server.service`:

```ini
[Unit]
Description=zun-rust-server
After=network-online.target tailscaled.service comfyui.service
Wants=network-online.target
Requires=comfyui.service

[Service]
Type=simple
User=zun
Group=zun
WorkingDirectory=/srv/zun
EnvironmentFile=/etc/zun/env
Environment="ZUN_CONFIG=/etc/zun/config.toml"
ExecStart=/usr/local/bin/zun-server
Restart=on-failure
RestartSec=3

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ReadWritePaths=/srv/zun
CapabilityBoundingSet=
AmbientCapabilities=
LockPersonality=true
RestrictRealtime=true
SystemCallArchitectures=native
SystemCallFilter=@system-service

[Install]
WantedBy=multi-user.target
```

ComfyUI should have its own similar unit; both are managed by systemd independently.

### Deployment workflow

From dev machine:

```bash
# Build release
cargo build --release

# Copy binary and deploy files
scp target/release/zun-server user@server:/tmp/
scp deploy/zun-server.service user@server:/tmp/

# On server
sudo mv /tmp/zun-server /usr/local/bin/zun-server
sudo chmod 755 /usr/local/bin/zun-server
sudo mv /tmp/zun-server.service /etc/systemd/system/

# First-time setup
sudo systemctl daemon-reload
sudo systemctl enable --now zun-server

# Subsequent updates
sudo systemctl restart zun-server
journalctl -u zun-server -f
```

### Cross-compilation (optional)

If the dev machine is x86_64 and the server is x86_64, no cross-compile needed — `cargo build --release` produces a portable binary. If the server is ARM (e.g., home-lab Raspberry Pi + external GPU):

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
```

Use `cross` (`cargo install cross`) if linking issues arise.

---

## Operational concerns

### Monitoring

For personal use, `journalctl -u zun-server -f` is sufficient. If monitoring becomes desired:

- Expose `/metrics` endpoint in Prometheus format via `axum-prometheus`
- Track: request count, latency histogram, queue depth, job duration histogram, failures by type
- Scrape with node_exporter + Prometheus, view in Grafana

Defer until there's a real operational problem to investigate.

### Log rotation

systemd-journald rotates logs by default (check `/etc/systemd/journald.conf` for `MaxRetentionSec` and `SystemMaxUse`). For most personal setups, the defaults are fine.

### Upgrades and schema changes

- Rust binary upgrades: build new release, `systemctl restart zun-server`. Downtime: a few seconds.
- Schema changes: add a new migration file, restart the service. `sqlx::migrate!().run(&pool)` is idempotent.
- **Never edit old migrations.** Each change gets a new timestamped file.

### Backup

Not essential for v1, but if desired:

```bash
#!/usr/bin/env bash
# /usr/local/bin/zun-backup
set -euo pipefail
DEST=/backup/zun/$(date +%F)
mkdir -p "$DEST"
sqlite3 /srv/zun/jobs.db ".backup $DEST/jobs.db"
rsync -a --delete /srv/zun/inputs/ "$DEST/inputs/"
rsync -a --delete /srv/zun/outputs/ "$DEST/outputs/"
rsync -a --delete /srv/zun/thumbs/ "$DEST/thumbs/"
# Retain 14 days
find /backup/zun -maxdepth 1 -type d -mtime +14 -exec rm -rf {} +
```

Scheduled via cron or systemd timer. Offsite: rsync to an external disk or borg/restic to cloud.

### Disaster recovery

If `jobs.db` is corrupted (extremely rare with WAL), delete it and restart the service. The app will re-create the schema. Past image files remain on disk but won't be visible through the API until rows are manually reconstructed (not worth doing for a personal app — just regenerate what you need).

### Capacity planning

- Disk: at ~3 MB per completed job, 100 GB holds ~30,000 jobs. Monitor with `df -h /srv`.
- SQLite: queries against `jobs` are O(log n) on indexed columns. Even 100,000 rows are trivially fast.
- GPU: one job at a time is the whole strategy. Queue depth is the only variable to watch; if jobs pile up you're not making generations faster, just waiting longer.

### Known limitations to document

- If the server process crashes mid-ComfyUI-execution, the ComfyUI-side job may continue running and produce an output that the server never picks up. Acceptable tradeoff for v1; the job will be marked failed and can be re-submitted.
- No distributed locking or multi-writer concerns — SQLite would need WAL + busy_timeout tuning, but a single process avoids this entirely.
- No rate limiting. Single user, single device. If abuse somehow occurs, the bearer token can be rotated.

---

## Notes for Claude Code sessions

When working on this Rust server project in Claude Code:

### Scope your requests to milestones

Use the milestones above as natural boundaries. Don't ask "build the whole server" — ask "implement Milestone 3, bearer auth middleware."

### Let Claude run Cargo

Claude Code can run `cargo check`, `cargo clippy`, `cargo test`, and parse output. Let it iterate on compile errors. `cargo check` is faster than `cargo build` for iteration.

### Check ComfyUI integration manually

Claude can't talk to your actual ComfyUI instance. For Milestone 5, use a wiremock-based fake for tests, and run the curl smoke test against the real ComfyUI yourself.

### Files to protect

- Never let Claude edit `/etc/zun/env` or commit the real token
- Never let Claude include the real `prompts.yaml` contents in code examples — those are your prompt engineering IP
- Review any changes to `deploy/zun-server.service` carefully (sandboxing directives)

### Stable decisions (do not change)

- Rust + axum + sqlx + SQLite, not Python/FastAPI or Go
- SQLite only, no Postgres
- Single binary, not microservices
- Pure-Rust dep tree — rustls everywhere, no OpenSSL, no `pkg-config` deps
- Bearer token only, no OAuth/JWT/sessions
- ComfyUI as a separate process (from project-zun, launched via `just serve`), never in-process ML
- Multiple workflow families supported, primary v1 target is `flux2_klein_edit`
- Workflow templates are opaque; substitution is string-placeholder, not node-ID patching
- No LoRA support in v1 (deferred — workflows keep their `LORA_PLACEHOLDER` slot but server ignores it)
- No text-to-image workflows in v1 (API shape requires an input image)
- No WebSockets in v1 (HTTP polling only — means `progress` field is effectively null)

### Style conventions

- `thiserror` for library errors, `anyhow` only inside binary boundaries if ever
- `tracing`, never `println!` or `log` crate
- `sqlx::query!` macros for compile-time checked queries once the schema stabilizes; runtime `sqlx::query` is fine during early milestones to avoid needing build-time DB metadata
- `Result<T, AppError>` everywhere; avoid `Box<dyn Error>`
- snake_case for JSON fields (matches Rust field names; no `serde(rename)` noise)
- Tabs: never. 4 spaces, rustfmt default

### Commit gate

Local `.git/hooks/pre-commit` (untracked, one-time setup) enforces:

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Both must pass, or the commit is rejected. Bypass with `git commit --no-verify` only when intentional (e.g., committing a WIP snapshot). Fresh clones re-create the hook by hand — or we version it under `.githooks/` + `git config core.hooksPath .githooks` once there's a second contributor.

### When in doubt

Prefer the boring choice. Prefer standard library over a crate. Prefer a function over a trait. Prefer a static config value over a dynamic one. This is a single-user personal service — complexity has no payoff here.
