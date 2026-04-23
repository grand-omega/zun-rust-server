# Notes for the Android Coder — Testing Against the Real Server

The Rust server (`zun-rust-server`, dev branch → main at commit `cee9ed2`) is feature-complete for v1 and matches the API contract defined in your `FluxApi.kt` / `Dto.kt`. All endpoints the app currently calls are implemented and tested, including the three image endpoints we previously flagged as Android-Milestone-9 work.

## How to run the server locally (Gentoo workstation)

From `~/Desktop/zun-rust-server`:

```bash
export ZUN_TOKEN=test-token-0123456789abcdef   # any 16+ char string
cargo run --release                             # or plain `cargo run` for iteration
```

You'll see something like:

```
INFO zun_rust_server: starting data_dir=./data bind=127.0.0.1:8080 comfy=http://127.0.0.1:8188
INFO zun_rust_server: prompts loaded n=3 path=./data/prompts.yaml
INFO zun_rust_server: workflow templates loaded n=10 dir=./data/workflows
INFO zun_rust_server: zun-rust-server listening addr=127.0.0.1:8080
```

Prerequisites:

1. ComfyUI running in `project-zun` at `127.0.0.1:8188` (`cd ~/Desktop/project-zun && just serve`). If it isn't up when the server starts, the server still runs — jobs submitted while ComfyUI is down just sit in `running` until a ComfyUI call times out (300 s), then go to `failed`.
2. Port `8080` free on the workstation.

Env overrides (all optional):

- `ZUN_BIND` — default `127.0.0.1:8080`. For LAN access set e.g. `ZUN_BIND=0.0.0.0:8080` (temporary — Tailscale/TLS lands in server milestone 7).
- `ZUN_DATA_DIR` — default `./data`. Where `jobs.db`, `inputs/`, `outputs/`, `thumbs/`, `workflows/` (symlink), `prompts.yaml` all live.
- `ZUN_COMFY_URL` — default `http://127.0.0.1:8188`.
- `RUST_LOG` — default `zun_rust_server=info,tower_http=info`. Set `tower_http=debug` for per-request access logs.

## Reaching the server from an Android device

For a first end-to-end smoke test, just put phone + workstation on the same Wi-Fi, bind the server to `0.0.0.0`, and use the workstation's LAN IP as the Android base URL:

```bash
export ZUN_BIND=0.0.0.0:8080
export ZUN_TOKEN=...
cargo run --release
```

Then in the Android build config, set the base URL to `http://<workstation-lan-ip>:8080/` and the token to the same `ZUN_TOKEN` string. Plain HTTP is fine for LAN; Tailscale/TLS comes later.

## API contract — what the server serves

Exactly what your `FluxApi.kt` declares. Field names below match `Dto.kt` verbatim.

| Method | Path | Auth | Body | Response |
|---|---|---|---|---|
| GET | `/api/health` | no | — | `{ "status": "ok", "version": "0.1.0" }` |
| GET | `/api/prompts` | yes | — | `[{ id, label, description? }]` (public projection — `text` and `workflow` are NOT leaked) |
| POST | `/api/jobs` | yes | multipart: `image` (jpeg/png, ≤ 20 MB) + `prompt_id` (string) | `201 { "job_id": "<uuid>" }` |
| GET | `/api/jobs/{id}` | yes | — | `{ id, status, prompt_id, prompt_label, progress, error, created_at, completed_at, width, height }` |
| GET | `/api/jobs?status=done&limit=30&before=<unix-s>` | yes | — | `[{ id, prompt_id, prompt_label, created_at, duration_seconds }]` (newest first, limit clamped to 1..=100) |
| DELETE | `/api/jobs/{id}` | yes | — | `204 No Content`; `404` if unknown. Removes DB row + input/output/thumb files. |
| GET | `/api/jobs/{id}/input` | yes | — | `image/jpeg` or `image/png` bytes; `Cache-Control: private, max-age=3600` |
| GET | `/api/jobs/{id}/result` | yes | — | `image/png` bytes; `409 { code: "not_ready" }` if status != "done" |
| GET | `/api/jobs/{id}/thumb` | yes | — | `image/jpeg` (400 px max side, lazy-generated on first request); `409` if not done |

### Error envelope

All error responses are JSON:

```json
{ "error": "human-readable message", "code": "<stable_slug>" }
```

Codes you'll see: `unauthorized` (401), `not_found` (404), `bad_request` (400), `invalid_prompt_id` (400), `not_ready` (409), `internal` (500). Your app currently only reads `error` — that's fine.

### Status values

`queued | running | done | failed`. Your ViewModel already treats `queued` and `running` identically; no change needed.

### Timestamps — IMPORTANT

**Unix seconds**, not milliseconds. Previously your `FakeJobRepository` used `System.currentTimeMillis()` (ms). The real server's `created_at` / `completed_at` are seconds. When you wire up `RealJobRepository`, parse timestamps with `Instant.ofEpochSecond(dto.created_at)` rather than the current `Date(dto.created_at)` (if that's what the Fake uses).

If you prefer, the server side can switch to ms — but the contract in the server plan has always been seconds, and our `before=<timestamp>` pagination parameter expects seconds. Cheaper to fix the Android side.

### `progress` field

Always `null` in v1. The server doesn't ship the ComfyUI WebSocket bridge yet — jobs just poll `queued → running → done` via HTTP. The Android code already handles `null` progress correctly (shows an indeterminate spinner), so no action. Real progress comes in a future server milestone.

### Request correlation

Every response includes an `x-request-id` header (UUID, server-generated if you don't send one). Feel free to send your own `x-request-id` header from the Android client if you want to correlate client-side logs with server logs — the server will echo it back.

### `Authorization` header redaction

Confirmed: the server never logs the bearer token. We use tower-http's `SetSensitiveRequestHeadersLayer` to mark Authorization sensitive, and the TraceLayer doesn't log header values anyway.

### Reading server logs

Server logs emit in two formats:

- **Pretty** (default when stderr is a TTY) — human-readable, colored.
- **JSON** (default when stderr is not a TTY, e.g. systemd / piped output) — one JSON object per line.

Force with `ZUN_LOG_FORMAT=pretty` or `ZUN_LOG_FORMAT=json`.

Every request runs inside a `request` span whose fields appear on every event during that request. A typical audit event looks like:

```json
{
  "level": "INFO",
  "target": "audit",
  "fields": {"event": "job.submitted", "job_id": "...", "prompt_id": "anime_style", "input_bytes": 482139},
  "span": {"id": "14f971bd-...", "method": "POST", "uri": "/api/jobs", "name": "request"}
}
```

Audit events you'll see per job: `job.submitted` → `job.running` → (`job.done` | `job.failed`) → `job.deleted`. Plus `auth.denied` on rejected requests (with `reason` + `path`).

Grep recipes when the app misbehaves:
```bash
# Everything for one request id:
journalctl -u zun-server | grep 14f971bd-

# All audit events in last hour:
ZUN_LOG_FORMAT=json cargo run | jq 'select(.target == "audit")'

# Job lifecycle for one job_id:
ZUN_LOG_FORMAT=json cargo run | jq 'select(.fields.job_id == "<uuid>")'

# 500s only:
ZUN_LOG_FORMAT=json cargo run | jq 'select(.level == "ERROR")'
```

## Smoke-test transcript

The server comes up with three dev-placeholder prompts baked in. Use any of:

```
anime_style
oil_painting
remove_bg
```

Minimal curl flow (adapt to fish/bash):

```fish
set TOKEN test-token-0123456789abcdef
set BASE http://127.0.0.1:8080
set IMG /home/doremy/Desktop/project-zun/inputs/(ls /home/doremy/Desktop/project-zun/inputs/ | head -1)

# 1. health (no auth)
curl -s $BASE/api/health | jq

# 2. prompt catalog
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/prompts | jq

# 3. submit
set JOB (curl -s -H "Authorization: Bearer $TOKEN" \
    -F "image=@$IMG;type=image/jpeg" \
    -F "prompt_id=anime_style" \
    $BASE/api/jobs | jq -r .job_id)
echo "submitted job=$JOB"

# 4. poll until done (klein edit takes ~7 s)
while true
    set S (curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/jobs/$JOB | jq -r .status)
    echo "status=$S"
    test "$S" = "done" -o "$S" = "failed"; and break
    sleep 1
end

# 5. fetch all three image variants
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/jobs/$JOB/input  -o /tmp/input.jpg
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/jobs/$JOB/result -o /tmp/result.png
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/jobs/$JOB/thumb  -o /tmp/thumb.jpg
file /tmp/input.jpg /tmp/result.png /tmp/thumb.jpg

# 6. gallery list (returns our just-completed job)
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/api/jobs?status=done&limit=10" | jq

# 7. delete (cleanup)
curl -si -H "Authorization: Bearer $TOKEN" -X DELETE $BASE/api/jobs/$JOB | head -1
```

A real end-to-end against my workstation just now clocked **~7 seconds** from submit to done (FLUX2 klein, single RTX 4070 Ti Super).

## Known gaps vs. Android expectations

None blocking. Items worth noting:

- **No TLS yet.** LAN HTTP only for now. Tailscale + `tailscale cert` rustls termination lands in server milestone 7. When that ships, the app's base URL flips from `http://100.x.x.x:8080/` to `https://<tailnet-hostname>/` with no DTO changes.
- **No rate limiting, no CORS.** Not needed for single-user Tailscale deployment.
- **One job at a time.** The worker is strictly serial — FLUX2 saturates the GPU. If you submit a second job while one is running, it sits in `queued` until the first finishes.
- **No per-prompt `negative`.** Dropped from DTO after the v1 scope decision (FLUX2 klein doesn't use it; Fill has it baked into the workflow JSON). If you've held a slot for `negative`, just ignore — the server will never send one.

## When you hit an issue

1. Check `echo $ZUN_TOKEN` matches what's in Android's `BuildConfig.API_TOKEN`. Mismatch = 401 on everything except `/api/health`.
2. If `POST /api/jobs` returns `{"code":"invalid_prompt_id",...}`, the `prompt_id` value doesn't appear in `/api/prompts`. Current valid values: `anime_style`, `oil_painting`, `remove_bg`.
3. If status stays `running` > 60 s, check ComfyUI is actually up (`curl localhost:8188/system_stats`). Worker times out at 300 s and marks the job `failed`.
4. For any 5xx, grep server stderr for the `x-request-id` from the response header — all logs during a request carry that id in a `request{id=...}` span.

If you find a contract mismatch with any field name, status code, or payload shape, paste the exact request + response and I'll fix the server side — the server-side DTOs are not sacred; the Android side is the client of record.
