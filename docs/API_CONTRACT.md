# API Contract — zun-rust-server v3.0.0

Single-user backend for ComfyUI image-edit jobs, driven by the Android client.
Plain HTTP behind a TLS-terminating reverse proxy; bearer-token auth.

## Conventions

- **Base**: all paths under `/api/v1`.
- **Auth**: every endpoint except `/api/v1/health` requires
  `Authorization: Bearer <token>`. A wrong/missing token returns 401; there is
  no per-IP rate limiting.
- **Encoding**: JSON in/out unless stated otherwise. Timestamps are unix
  seconds (i64). IDs: jobs are UUIDv4 strings; inputs and prompts are i64 row IDs.
- **Errors**: uniform JSON shape:
  ```json
  { "error": "<message>", "code": "<machine_code>" }
  ```
  Codes: `unauthorized` (401), `not_found` (404), `bad_request` (400),
  `payload_too_large` (413), `not_ready` (409), `need_upload` (409),
  `internal` (500).
  `need_upload` carries extra fields — see POST `/jobs`.
- **Limits**: multipart upload ≤ 20 MiB (over the limit returns 413
  `payload_too_large`, not 400); `prompt_text` ≤ 8 KiB; per-request
  timeout 120 s.

---

## Health & capabilities

### `GET /api/v1/health` — unauthenticated

```json
{
  "status": "ok",
  "version": "3.0.0",
  "disk": { "data_bytes": 1234567 }
}
```

`disk.data_bytes` may be `null` if the walk failed; cached for ~60 s.

### `GET /api/v1/capabilities`

```json
{
  "version": "3.0.0",
  "features": {
    "image_edit": true,
    "auto_mask": false,
    "lora": false,
    "reference_image": false,
    "manual_mask": false,
    "text_to_image": false
  },
  "workflows": [
    {
      "name": "flux2_klein_edit",
      "display_name": "FLUX 2 klein",
      "kind": "image_edit",
      "requires_input_image": true,
      "experimental": false,
      "default": true,
      "runtime": "comfyui",
      "pipeline": null,
      "model_path": null,
      "dtype": null,
      "offload_mode": null,
      "default_steps": null,
      "default_width": null,
      "default_height": null,
      "loaded": true,
      "supported": true,
      "placeholders": [
        "PROMPT_PLACEHOLDER",
        "INPUT_IMAGE_PLACEHOLDER",
        "FILENAME_PREFIX_PLACEHOLDER",
        "SEED_PLACEHOLDER"
      ],
      "warning": null,
      "reason": null
    }
  ]
}
```

Only `flux2_klein_edit` is wired up in this build (the 9B-KV/Diffusers workflow
was removed). The set is baked in at compile time — there's no runtime knob.

---

## Custom prompts

Catalog of saved prompt presets. Soft-deleted rows do not appear in any read.

### `POST /api/v1/prompts` → `201`

```json
{
  "label": "anime",
  "description": "optional",
  "text": "make it anime",
  "workflow": "flux2_klein_edit",
  "timeout_seconds": 90
}
```

`label` and `text` must be non-empty after trim; `text` ≤ 8 KiB; `workflow`
must appear in `/capabilities` with `supported: true`. `timeout_seconds` is
optional; falls back to `60`.

Response: full prompt row (see GET).

### `GET /api/v1/prompts`

```json
{
  "items": [
    {
      "id": 1,
      "label": "anime",
      "description": null,
      "text": "make it anime",
      "workflow": "flux2_klein_edit",
      "timeout_seconds": 90,
      "created_at": 1746700000,
      "updated_at": 1746700000
    }
  ]
}
```

### `GET /api/v1/prompts/{id}`

Single prompt row (same shape as the items in list). 404 if missing/deleted.

### `PATCH /api/v1/prompts/{id}`

Sparse update. All fields optional; omitting a field leaves it unchanged.
`description` and `timeout_seconds` accept explicit `null` to clear.

```json
{ "label": "anime v2", "text": "...", "timeout_seconds": null }
```

Response: updated prompt row.

### `DELETE /api/v1/prompts/{id}` → `204`

Soft delete. There is no restore endpoint for prompts.

---

## Jobs

A job pairs an input image (referenced by content hash) with either a stored
prompt or free-form prompt text, and runs it through one workflow.

### `POST /api/v1/jobs` → `202`

Two content types accepted; pick by `Content-Type`.

**JSON variant** (`application/json`) — used when the client is sure the
input image is already cached server-side.

```json
{
  "input_sha256": "<64 lowercase hex>",
  "input_name": "optional original filename",
  "prompt_id": 1,
  "prompt_text": null,
  "workflow": null
}
```

**Multipart variant** (`multipart/form-data`) — fields:
- `image`: the input bytes. `Content-Type` must be `image/jpeg` or `image/png`.
- `input_sha256`: SHA-256 of `image`. Server verifies bytes match.
- `input_name` *(optional)*: original filename for display.
- `prompt_id` *(optional)*: integer.
- `prompt_text` *(optional)*: free-form prompt.
- `workflow` *(optional)*: required when `prompt_text` is used.

Validation rules (both variants):
- Exactly one of `prompt_id` or `prompt_text` must be set.
- When `prompt_text` is set, `workflow` is required and must be supported;
  `prompt_text` is non-empty after trim and ≤ 8 KiB.
- `prompt_id` must reference a non-deleted prompt; its `workflow` is used.
- `input_sha256` must be 64 lowercase hex chars.

**202 response**:
```json
{ "job_id": "<uuid>", "input_id": 42 }
```
`Location: /api/v1/jobs/<uuid>` is set.

**409 `need_upload`** — JSON variant only, when no cached file backs
`input_sha256`:
```json
{
  "error": "input cache miss; re-upload required",
  "code": "need_upload",
  "need_upload": true,
  "input_id": 42
}
```
`input_id` is `null` if the hash has never been seen. Client should retry as
multipart with the bytes.

### `GET /api/v1/jobs`

Query parameters:
- `status`: one of `queued`, `running`, `done`, `failed`, `cancelled`.
- `input_id`: filter to jobs for a specific input.
- `cursor`: opaque cursor from the previous page's `next_cursor`.
- `limit`: 1–200, default 50.

```json
{
  "items": [
    {
      "id": "<uuid>",
      "input_id": 42,
      "source_prompt_id": 1,
      "prompt_text": "make it anime",
      "workflow": "flux2_klein_edit",
      "seed": 1234567890,
      "status": "done",
      "progress": 1.0,
      "created_at": 1746700000,
      "completed_at": 1746700007,
      "duration_seconds": 7
    }
  ],
  "next_cursor": "<opaque>"
}
```

Soft-deleted jobs are excluded. Order is `(created_at DESC, id DESC)`;
pagination is keyset on that pair. `next_cursor` is `null` on the last page.

### `GET /api/v1/jobs/{id}`

Query parameters:
- `wait` (optional): long-poll window in seconds, capped at 30. When set,
  the response is held open until the job's `status` or `progress`
  changes, or the window elapses (then the current state is returned).
  Poll with `wait=25` in a loop instead of hammering short GETs.

```json
{
  "id": "<uuid>",
  "input_id": 42,
  "source_prompt_id": 1,
  "prompt_text": "make it anime",
  "workflow": "flux2_klein_edit",
  "seed": 1234567890,
  "status": "done",
  "progress": 1.0,
  "queue_position": null,
  "error": null,
  "created_at": 1746700000,
  "started_at": 1746700001,
  "completed_at": 1746700008,
  "width": 1024,
  "height": 1024,
  "metadata": { /* free-form sidecar from the worker, if present */ }
}
```

`error` is the failure message when `status == "failed"`. `metadata` is the
parsed `<output>.json` sidecar emitted alongside the result image; `null` if
absent or unreadable. `progress` is 0.0–1.0, parsed live from ComfyUI while
the job runs (done ⇒ 1.0). `queue_position` is the number of queued jobs the
worker will pick first — `0` means next up; `null` unless `status == "queued"`.

### `DELETE /api/v1/jobs/{id}` → `204`

Soft delete. The job stays in the DB, hidden from list/get/result.

### `POST /api/v1/jobs/{id}/restore` → `204`

Reverses a soft delete. 404 if the row was never deleted.

### `POST /api/v1/jobs/{id}/cancel` → `204`

Atomically transitions `queued`/`running` → `cancelled` and best-effort
issues a ComfyUI `/interrupt`. 404 if the job is in any other status.

### `GET /api/v1/jobs/{id}/result`

Streams the full-resolution output (PNG/JPEG, content-type matches the file).
- `200` with body, `Content-Length`, `ETag`, `Cache-Control: private, max-age=3600`.
- `304 Not Modified` when `If-None-Match` matches.
- `409 not_ready` if the job is not yet `done`.
- `404` if the job (or its file) is missing.

### `GET /api/v1/jobs/{id}/thumb`

400 px thumbnail. Sends AVIF when `Accept` includes `image/avif`; otherwise
sends JPEG. Same response shape, headers, and `not_ready` semantics as
`/result`, plus `Vary: Accept`. Generated lazily on first request.

### `GET /api/v1/jobs/{id}/preview`

~1280 px image sized for full-screen phone viewing. Same AVIF/JPEG negotiation
and caching semantics as `/thumb`.

---

## Inputs (read-only)

Inputs are content-addressed by SHA-256; rows persist after the cached file
is purged so old jobs keep a referenceable input.

### `GET /api/v1/inputs/{id}`

```json
{
  "id": 42,
  "sha256": "<hex>",
  "available": true,
  "original_name": "photo.jpg",
  "content_type": "image/jpeg",
  "size_bytes": 482133,
  "width": 4032,
  "height": 3024,
  "created_at": 1746690000,
  "last_used_at": 1746700000
}
```

`available: false` means the row exists but the file has been purged — a new
job for this hash will hit `need_upload`.

### `GET /api/v1/inputs/{id}/file`

Streams the cached input bytes. `200` / `304` / `404`, same caching headers
as `/result`. `404` if the file was purged (`available: false`).

---

## Status lifecycle

```
queued ──▶ running ──▶ done
   │           │
   │           └──▶ failed
   └──────────────▶ cancelled  (from queued or running, via /cancel)
```

Terminal: `done`, `failed`, `cancelled`. Soft-delete (`deleted_at`) is
orthogonal to status — any state can be hidden.
