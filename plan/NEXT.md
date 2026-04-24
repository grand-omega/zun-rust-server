# zun-rust-server v2 — Implementation Plan

> Rebuild of the data and auth layers for multi-device, multi-user-ready
> operation. Starts simple (one user, no passkeys) and ends with a
> production-grade passkey-based auth layer. Every phase is a committable,
> testable increment that leaves the system working.
>
> Audience: a Rust engineer landing into this repo cold. PLAN.md describes
> the current v1. This file describes what v2 looks like and how to build it.

---

## Table of contents

1. [Context](#context)
2. [First principles](#first-principles)
3. [v2 scope](#v2-scope)
4. [Architectural decisions (the "why")](#architectural-decisions)
5. [Data model](#data-model)
6. [Filesystem layout](#filesystem-layout)
7. [API surface](#api-surface)
8. [Implementation phases](#implementation-phases)
9. [Coordination with project-zun](#coordination-with-project-zun)
10. [Explicit non-goals](#explicit-non-goals)
11. [Open decisions](#open-decisions)

---

## Context

zun-rust-server today (v1, see `PLAN.md`):

- Axum HTTP server + single-task worker draining a SQLite-backed job queue.
- Single user, single bearer token configured in `config.toml`.
- Jobs reference an input file directly (`jobs.input_path`); the input is uploaded as multipart with each submission.
- Prompts live in `prompts.toml` on the server, a curated catalog. A reserved `__custom__` id exists for free-text prompts.
- ComfyUI integration over HTTP + WebSocket (ws completion event already wired; `src/comfy.rs::await_completion`).
- Plain HTTP (no TLS currently). On Tailscale.

What changes in v2:

- **Data ownership is per-user** with enforced scoping. Designed for one user now but ready for friends/family later (`user_id` FK everywhere; no code that can cross users).
- **Inputs become a content-addressed server-side cache**, not persistent user assets. User's photos live on their phone; the server caches them as needed.
- **Outputs are the user-curated artifact.** Gallery + soft delete operate on outputs.
- **Predefined `prompts.toml` catalog goes away.** All prompts are per-user DB rows.
- **Regeneration works** by injecting a random seed per job (requires a contract change in `project-zun`).
- **Auth becomes passkey-based (WebAuthn) with opaque session tokens.** Lands last; earlier phases preserve the existing bearer-token path.

---

## First principles

The design below falls out of a short list of principles. When in doubt during implementation, check against these.

1. **Server owns what the user creates, not what they already have.** Outputs, custom prompts, job history: server is source of truth. Source photos: server is a cache; the phone is authoritative.
2. **Thin client.** Android should do as little as possible. Any logic that can live on the server does.
3. **Every data access is user-scoped at the type level.** Data-access functions take `UserId` as a mandatory parameter. There is no code path that can return another user's row by accident.
4. **Content-addressed input cache.** Inputs are keyed by sha256 per user. Re-submitting the same photo is free (no re-upload). Cache is aggressively purgeable.
5. **Soft deletes for recoverability.** User-facing deletes set `deleted_at`; a background task hard-deletes after 30 days. User gets "oh shit, undo" out of the box.
6. **Serial GPU execution.** One job at a time. FLUX saturates. No concurrency is correct at this scale.
7. **Explicit over implicit.** Seeds recorded on every job. Workflow names snapshotted on every job. "What was this output made from?" is always answerable from the row.
8. **Phased migration preserves a working system.** No big-bang rewrite. Every phase leaves the app functional for its current user.

---

## v2 scope

### MUST have

- Per-user data scoping (`user_id` FK on all user-owned tables).
- Inputs-as-cache with sha256 dedup.
- Custom prompts as per-user DB rows; no server-side catalog file.
- Per-job random seed injection for regeneration.
- Gallery listing of outputs, paginated.
- Soft delete on outputs (`DELETE /api/v1/jobs/{id}`).
- Passkey-based auth (WebAuthn + opaque sessions).
- TLS (required for passkeys; use Tailscale certs).

### SHOULD have (but can ship a phase later)

- Admin CLI: `zun-admin user create`, `zun-admin user delete`, `zun-admin enroll`, `zun-admin seed-prompts`.
- Auto-purge cron for stale input cache files (>30d unused).
- Auto-purge cron for soft-deleted jobs (>30d deleted).
- Session management endpoints (list / revoke / logout-all).
- Starter prompt seeding at user creation time.

### OUT OF SCOPE for v2

- User settings / preferences table. (Deferred to v3.)
- Favorites / tags / collections / search.
- Streaming per-step progress to clients.
- Public web frontend (but schema and auth must be web-frontend-compatible).
- Sharing between users.
- OAuth, SSO, magic links, password-based login.
- Batch-grouped submissions as a first-class concept (client orchestrates via N sequential calls).
- Cross-user input dedup (dedup is per-user only, for privacy).

---

## Architectural decisions

### Why passkeys + opaque sessions (not passwords, not JWT)

- Passkeys are phishing-resistant by design; private key never leaves the authenticator.
- Proton Pass (already in user's stack) syncs passkeys across devices, killing the "lost device = lost credential" problem.
- Opaque session tokens (not JWT) because we need revocation to actually work. JWT revocation requires a blocklist — at which point you've built a worse session system.
- Access token (15 min) + refresh token (90 days, rotating) is the industry-standard session pattern. Rotation enables theft detection.

### Why TLS (even on Tailscale)

- WebAuthn spec-mandates HTTPS (except `localhost`). Browsers and Android Credential Manager enforce this regardless of network layer.
- Tailscale's `tailscale cert <hostname>` produces a free, auto-renewable Let's Encrypt cert for the tailnet hostname. Near-zero operational cost.

### Why inputs-as-cache, not inputs-as-assets

- Source photos already exist on the user's phone. Replicating them as a "server inputs library" duplicates data the user doesn't care to manage.
- Outputs are the valuable artifact — the *new* thing the user created. Those are what curation should target.
- Content-addressing by sha256 makes re-generation cheap (zero network cost after first upload) and makes dedup correct.
- Aggressive purging is safe because the phone always has the original.

### Why no server-side `prompts.toml`

- If custom prompts work per-user, a separate server catalog is redundant.
- Eliminates the merge/union logic between "server-provided" and "user-defined" prompts.
- Eliminates the `__custom__` special-case id.
- First-run UX handled via admin CLI seeding from a starter TOML template at user-creation time (not a live catalog).

### Why `user_id` on a table with a single user

- The abstraction cost is ~4 extra characters per query (`WHERE user_id = ?`).
- The migration cost later is huge if you don't have it.
- It's also the enforcement mechanism for "never leak another user's data" — habits formed now apply forever.

### Why random seed per job (not hardcoded in workflow JSON)

- "Regenerate" must produce a different result, which requires varying the seed.
- Recording the seed on the `jobs` row makes regeneration *optionally* reproducible if you want to use the same seed deliberately later.
- Requires a one-time contract change in `project-zun/doc/WORKFLOWS.md` — adding `SEED_PLACEHOLDER` — and a one-line edit to each sampler node in each workflow JSON.

### Why soft deletes

- Users expect "oh no, undo." Soft delete + scheduled purge is the standard way to give it to them.
- File deletion is deferred to a cron, which also batches IO efficiently.

---

## Data model

Full target schema after all phases land. Fields marked `(v1)` exist today and stay; `(new)` are added in v2.

```sql
-- ════════ auth layer (Phase 7–8) ════════

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    disabled_at   INTEGER
);

CREATE TABLE passkeys (
    id            INTEGER PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id BLOB NOT NULL UNIQUE,
    passkey_json  TEXT NOT NULL,          -- webauthn_rs::Passkey serialization
    nickname      TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);

CREATE TABLE sessions (
    id                  INTEGER PRIMARY KEY,
    user_id             INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    access_token_hash   BLOB NOT NULL UNIQUE,  -- sha256(token)
    refresh_token_hash  BLOB NOT NULL UNIQUE,
    access_expires_at   INTEGER NOT NULL,
    refresh_expires_at  INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    last_used_at        INTEGER NOT NULL,
    user_agent          TEXT,
    revoked_at          INTEGER,
    created_from_passkey_id INTEGER REFERENCES passkeys(id)
);
CREATE INDEX sessions_access  ON sessions(access_token_hash)  WHERE revoked_at IS NULL;
CREATE INDEX sessions_refresh ON sessions(refresh_token_hash) WHERE revoked_at IS NULL;

CREATE TABLE webauthn_challenges (
    id          INTEGER PRIMARY KEY,
    state_json  TEXT NOT NULL,
    purpose     TEXT NOT NULL,             -- 'register' | 'authenticate' | 'enroll'
    user_id     INTEGER REFERENCES users(id),
    expires_at  INTEGER NOT NULL
);

CREATE TABLE enrollment_tokens (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash  BLOB NOT NULL UNIQUE,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
);

-- ════════ content layer (Phases 1–5) ════════

CREATE TABLE inputs (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    sha256          TEXT NOT NULL,            -- hex, 64 chars
    path            TEXT,                     -- NULL means file purged; row retained for FK stability
    original_name   TEXT,
    content_type    TEXT,
    size_bytes      INTEGER,
    width           INTEGER,
    height          INTEGER,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER NOT NULL,
    UNIQUE (user_id, sha256)
);
CREATE INDEX inputs_user_last_used ON inputs(user_id, last_used_at);

CREATE TABLE custom_prompts (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label           TEXT NOT NULL,
    description     TEXT,
    text            TEXT NOT NULL,
    workflow        TEXT NOT NULL,            -- stem matching a file in data/workflows/
    timeout_seconds INTEGER,                  -- NULL = use server default
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deleted_at      INTEGER
);
CREATE INDEX custom_prompts_user ON custom_prompts(user_id, deleted_at);

CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,         -- UUID v4
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    input_id        INTEGER NOT NULL REFERENCES inputs(id),
    prompt_id       INTEGER REFERENCES custom_prompts(id),  -- nullable
    prompt_text     TEXT,                     -- fallback when prompt_id is null; exactly one of prompt_id/prompt_text must be set
    workflow        TEXT NOT NULL,            -- snapshot of effective workflow at submit time
    seed            INTEGER NOT NULL,         -- random u64-as-i64 set at submit
    status          TEXT NOT NULL,            -- queued | running | done | failed
    comfy_prompt_id TEXT,
    output_path     TEXT,
    thumb_path      TEXT,
    error_message   TEXT,
    width           INTEGER,
    height          INTEGER,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    deleted_at      INTEGER
);
CREATE INDEX jobs_user_status  ON jobs(user_id, status, created_at DESC);
CREATE INDEX jobs_user_active  ON jobs(user_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX jobs_input        ON jobs(input_id);
CREATE INDEX jobs_status_queue ON jobs(status, created_at) WHERE status IN ('queued', 'running');
```

### Invariants

- `jobs.user_id == inputs.user_id` (the input belongs to the same user as the job). Enforce in handlers; SQLite FKs alone don't express this.
- `jobs.user_id == custom_prompts.user_id` when `jobs.prompt_id IS NOT NULL`. Same enforcement.
- Exactly one of `(jobs.prompt_id, jobs.prompt_text)` is non-null. Enforce with a `CHECK` constraint or in the handler.
- `inputs.path IS NULL` means file is purged but row is retained. Submit flow handles re-uploading to the same row.

---

## Filesystem layout

```
data/
├── jobs.db
├── workflows/                         # admin-curated, shared across users (symlink to project-zun)
└── users/
    └── <user_id>/
        ├── cache/
        │   └── inputs/                # purgeable; files named <sha256>.<ext>
        │       └── <64 hex>.jpg
        ├── outputs/                   # user-curated, persistent
        │   └── <job_id>_00001_.png
        └── thumbs/
            └── <job_id>.jpg
```

- `cache/` prefix signals "anything here can be deleted anytime."
- Input files are named by content hash, not by input_id. This makes dedup at the filesystem level also true: the same sha256 always maps to the same filename.
- Output files keep ComfyUI's naming convention (`<job_id>_<seq>_.png`).
- A single path-building helper (`user_data_path(user_id, subdir, filename)`) is the only place paths are assembled. It refuses filenames containing `..` or `/` and always roots under `data/users/<user_id>/`. Centralizes traversal prevention.

---

## API surface

All authenticated endpoints are under `/api/v1/` and require a valid access token (`Authorization: Bearer zun_at_...`). `/api/v1/health` and `/api/v1/auth/*` are the exceptions.

### Jobs (the primary surface)

```
POST   /api/v1/jobs
  Content-Type: multipart/form-data  (required if server doesn't have the input cached)
  Content-Type: application/json     (used when input is already cached)

  Common fields (multipart or JSON):
    input_sha256:  <hex>                   # REQUIRED
    input_name:    <string>                # original filename (optional, for display)
    prompt_id:     <int>                   # one of prompt_id / prompt_text, not both
    prompt_text:   <string>
    workflow:      <string>                # REQUIRED iff prompt_text is set

  Multipart only:
    image:         <file bytes>

  Responses:
    202 Accepted       { job_id, input_id }
    409 Conflict       { need_upload: true, input_id }   # hash known but file purged; client re-uploads
    400 Bad Request    validation errors
    401 Unauthorized
    413 Payload Too Large

GET    /api/v1/jobs                        list, paginated
  Query params:
    status=done|running|queued|failed      (optional)
    input_id=<int>                         (optional, filter by input)
    cursor=<opaque>                        (optional)
    limit=<int, default 50, max 200>
  Response:
    { items: [JobSummary], next_cursor: <opaque or null> }

GET    /api/v1/jobs/{id}                   single job; 404 if not user's or deleted
DELETE /api/v1/jobs/{id}                   soft delete; 204
POST   /api/v1/jobs/{id}/restore           undo soft delete within 30 days; 204 or 404
GET    /api/v1/jobs/{id}/result            output PNG bytes; 404 unless status=done
GET    /api/v1/jobs/{id}/thumb             400px JPEG; lazy-generated
```

### Inputs (read-only for display)

```
GET    /api/v1/inputs/{id}                 metadata
  Response includes { available: bool }    # true iff file still in cache
GET    /api/v1/inputs/{id}/file            bytes; 404 if purged
```

### Custom prompts

```
POST   /api/v1/prompts                     create
  { label, description?, text, workflow, timeout_seconds? }
  201 { id, ... }

GET    /api/v1/prompts                     list user's non-deleted prompts
GET    /api/v1/prompts/{id}
PATCH  /api/v1/prompts/{id}                partial update (label, description, text, workflow, timeout_seconds)
DELETE /api/v1/prompts/{id}                soft delete
```

### Auth (Phase 8)

See [Phase 8](#phase-8-passkey-auth-plus-sessions) for detail. Shape:

```
POST /api/v1/auth/enroll/start
POST /api/v1/auth/enroll/finish
POST /api/v1/auth/login/start
POST /api/v1/auth/login/finish
POST /api/v1/auth/refresh
POST /api/v1/auth/logout
POST /api/v1/auth/logout-all
GET  /api/v1/auth/sessions
POST /api/v1/auth/sessions/{id}/revoke
GET  /api/v1/auth/passkeys
POST /api/v1/auth/passkeys/register/start
POST /api/v1/auth/passkeys/register/finish
DELETE /api/v1/auth/passkeys/{id}
GET  /api/v1/me
```

### Health

```
GET  /api/v1/health                        (unauthenticated)
  { status, version, comfy: { ok, last_ok_at, consecutive_failures } }
```

---

## Implementation phases

Each phase is a committable PR. Order matters — later phases assume earlier ones. Each phase leaves tests passing and the app usable.

### Phase 1 — User table + user_id scoping (no auth changes yet)

**Goal:** all data-access is user-scoped, but auth is unchanged (still static bearer token from `config.toml`).

**Rationale:** decouples the discipline of "user_id everywhere" from the complexity of passkey auth. Once discipline is in place, swapping auth is just swapping middleware.

**Tasks:**

1. New migration: `users` table (schema above, minus `disabled_at` for now if you want).
2. Middleware reads `config.token` and, on success, attaches `Extension(UserId(1))` instead of just passing through. At startup, ensure a seed row exists in `users` (id=1, username="admin", display_name from config or literal "admin").
3. Add `user_id` FK to `jobs` table via migration. Backfill existing rows to `user_id=1`.
4. Introduce `UserId(i64)` newtype in `src/state.rs` or `src/auth/mod.rs`.
5. Audit every query in `src/handlers.rs`, `src/images.rs`, `src/worker.rs`:
   - Every SELECT/UPDATE/DELETE on `jobs` gains `AND user_id = ?` (or includes it in `INSERT`).
   - Handlers take `Extension<UserId>` as the first axum extractor.
   - Helper fns in `src/db.rs` take `UserId` as a mandatory parameter (don't accept it implicitly).
6. Rebuild the router with `UserId`-requiring signatures; handlers get the extension automatically.

**Files touched:**
- New: `migrations/XXX_users.sql`
- Modified: `src/auth.rs`, `src/handlers.rs`, `src/images.rs`, `src/worker.rs`, `src/state.rs`, `tests/common/mod.rs` (seed a user for tests), all integration tests.

**Verification:**
- All existing tests pass.
- Manually: submit a job with the existing token, observe the new `user_id = 1` column.
- Verify no SELECT on `jobs` lacks a `user_id` filter: `grep -n "FROM jobs" src/ | grep -v user_id`.

**Out of scope for this phase:**
- Custom prompts table (Phase 4).
- Inputs table (Phase 3).
- Any auth changes (Phase 8).

---

### Phase 2 — Migrate filesystem layout to per-user directories

**Goal:** files live under `data/users/<user_id>/...`; all path assembly goes through one helper.

**Tasks:**

1. Add `src/paths.rs` with `user_data_path(user_id: UserId, subdir: &str, filename: &str) -> PathBuf`. Rejects `..` and `/` in `filename`. Creates parent dirs on write.
2. Update worker's input read and output write paths to use the helper.
3. Write a one-shot migration binary `src/bin/migrate_v1_to_v2.rs`:
   - For each job in DB, move files from old flat layout to new per-user layout.
   - Update DB paths in the same transaction.
   - Dry-run mode + apply mode.
4. Document in README: run `cargo run --bin migrate_v1_to_v2 -- --apply` as the upgrade step.

**Files touched:**
- New: `src/paths.rs`, `src/bin/migrate_v1_to_v2.rs`.
- Modified: `src/worker.rs`, `src/images.rs`.

**Verification:**
- Dry-run prints expected moves without touching the filesystem.
- After apply: old paths are empty, new paths contain files, DB reflects new paths.
- Tests updated to use per-user tempdir layouts.

---

### Phase 3 — Inputs table + content-addressed cache

**Goal:** inputs become their own resource, identified by content hash. `jobs.input_path` → `jobs.input_id` FK.

**Tasks:**

1. Migration: create `inputs` table. Migration populates one `inputs` row per unique `(user_id, sha256)` pulled from existing `jobs`. Drop `jobs.input_path`, add `jobs.input_id NOT NULL`.
   - **Important:** compute sha256 of each existing input file during migration to populate the new table. This means the migration needs to read files, not just manipulate rows.
2. Rewrite `POST /api/v1/jobs` handler:
   - Accept both multipart (file + hash) and JSON (hash-only).
   - Hash-check-then-upload flow:
     ```
     IF (user_id, sha256) exists AND path is not null:
       use existing input_id; bump last_used_at
     ELSE IF hash-only and no row OR row with null path:
       return 409 { need_upload: true, input_id? }
     ELSE (multipart with bytes):
       write to data/users/<uid>/cache/inputs/<sha256>.<ext>
       upsert inputs row; set path
     ```
3. Add `GET /api/v1/inputs/{id}`, `GET /api/v1/inputs/{id}/file`.
4. Worker reads input via `SELECT path FROM inputs WHERE id = ?`, not from `jobs.input_path`.

**Files touched:**
- New migration.
- Modified: `src/handlers.rs` (submit handler is the big one), `src/worker.rs`, `src/images.rs`, `src/db.rs`, tests.

**Verification:**
- Existing jobs continue to work after migration.
- Submit the same photo twice by hash → second submission returns same `input_id`, no re-upload.
- Submit the same photo as multipart twice → same, file is written once (dedup at the fs layer because filename is the hash).
- 409 flow: purge an input file manually, re-submit with hash only, expect 409.

---

### Phase 4 — Custom prompts table; drop `prompts.toml`

**Goal:** all prompts are per-user DB rows. Server has no catalog file.

**Tasks:**

1. Migration: create `custom_prompts` table. On migration, seed existing `prompts.toml` entries into user 1's `custom_prompts` rows (one-shot, inside the migration).
2. Delete `src/prompts.rs`, `data/prompts.toml`, `data/prompts.example.toml`, the `prompts::load` and `prompts::inject_custom` code paths.
3. Remove `custom_prompt_workflow` from `Config` (no more `__custom__` injection).
4. Replace the `AppState.prompts` field (was `Arc<HashMap>`) — prompts are now per-request DB lookups. Small perf hit, negligible at this scale; simpler code.
5. New CRUD handlers for `/api/v1/prompts`.
6. Submit handler resolves the prompt:
   - If `prompt_id` is set: `SELECT * FROM custom_prompts WHERE id = ? AND user_id = ?`.
   - If `prompt_text` is set: use that + the `workflow` from the request directly.
   - Snapshot the resolved `workflow` name onto `jobs.workflow`.
7. Admin CLI (new): `zun-admin seed-prompts <username> --from <toml>` — reads a TOML file with the same shape `prompts.toml` used, inserts rows into the user's table. Create a `starter_prompts.toml` in the repo root (not loaded at runtime, purely a seed source).

**Files touched:**
- Deleted: `src/prompts.rs`, `data/prompts.toml`, `data/prompts.example.toml`.
- New: `src/bin/zun_admin.rs` (subcommand: `seed-prompts`), `starter_prompts.toml`.
- Modified: `src/handlers.rs`, `src/worker.rs`, `src/state.rs`, `src/config.rs`, `src/main.rs`, tests.

**Verification:**
- Existing jobs (with migrated prompts) continue running.
- Create / list / update / delete a prompt via API.
- Submit with `prompt_id` → picks correct text + workflow.
- Submit with `prompt_text + workflow` → runs free-text.

---

### Phase 5 — Seed injection for regeneration

**Goal:** every submit generates a unique, recorded seed; "regenerate" is just another submit.

**Coordination prerequisite:** update `project-zun` first. See [Coordination](#coordination-with-project-zun).

**Tasks:**

1. Add `SEED` placeholder constant in `src/workflow.rs` (`pub const SEED: &str = "SEED_PLACEHOLDER";`).
2. Add `seed: i64` parameter to `build_edit_workflow`, add the seed substitution.
3. Migration: `ALTER TABLE jobs ADD COLUMN seed INTEGER NOT NULL DEFAULT 0` (backfill zero for historical rows; acceptable because they've already run).
4. In `src/handlers.rs` submit handler: generate `let seed: i64 = rand::random();` at job creation, persist on the row.
5. Worker reads `seed` from the job row, passes to `build_edit_workflow`.
6. Include `seed` in job GET responses.

**Files touched:**
- Modified: `src/workflow.rs`, `src/worker.rs`, `src/handlers.rs`, new migration.

**Verification:**
- Two submits of the same `(input, prompt)` yield different output images (visual check with a real workflow).
- `seed` appears in the job row and in API responses.
- Workflow templates with `SEED_PLACEHOLDER` patch correctly; templates without it fail to substitute (caught by existing placeholder tests).

---

### Phase 6 — Pagination + soft delete + cache purge

**Goal:** gallery scales; deletes are reversible; inputs cache doesn't grow forever.

**Tasks:**

1. Migration: add `deleted_at` to `jobs` and `custom_prompts` (inputs already has the scheme via nullable `path`).
2. Rewrite `GET /api/v1/jobs` to use cursor-based pagination:
   - Cursor is `base64(json({created_at, id}))`.
   - Query: `WHERE user_id = ? AND deleted_at IS NULL AND (created_at, id) < (?, ?) ORDER BY created_at DESC, id DESC LIMIT ?`.
   - Return `{ items, next_cursor }` where `next_cursor` is null when no more pages.
3. Rewrite `DELETE /api/v1/jobs/{id}` to set `deleted_at = now()` (keep 204 response).
4. Add `POST /api/v1/jobs/{id}/restore`.
5. Background purge task (spawn in `main.rs`, similar to `comfy_monitor::spawn`):
   - Daily tick.
   - Hard-delete soft-deleted jobs older than 30 days: remove output + thumb files, DELETE row.
   - Nullify `inputs.path` and delete cache files where `last_used_at < now() - 30d` and no active job references them.
6. Admin CLI: `zun-admin purge --dry-run` exposes the same logic manually.

**Files touched:**
- New: `src/purge.rs`, new migration.
- Modified: `src/handlers.rs`, `src/main.rs`, `src/bin/zun_admin.rs`, tests.

**Verification:**
- Paginated list returns stable results under concurrent inserts (cursor is position-stable).
- Delete → restore within 30 days succeeds.
- Dry-run purge prints what it would delete.
- Real purge run removes expected files.

---

### Phase 7 — TLS via Tailscale certs

**Goal:** server speaks HTTPS. Prerequisite for Phase 8 (passkeys require HTTPS).

**Tasks:**

1. Add `axum-server = "0.7"` (or current) and `tokio-rustls` to `Cargo.toml`.
2. Config: `tls_cert_path: Option<PathBuf>`, `tls_key_path: Option<PathBuf>` in `Config`. If both set, serve HTTPS; otherwise serve HTTP (dev fallback).
3. In `main.rs`: choose `axum::serve` vs `axum_server::bind_rustls` based on config.
4. Document in README: `tailscale cert <hostname>` and pointing the config at the resulting files. Include a systemd timer snippet for automatic renewal (every 60 days).
5. Health endpoint remains reachable on HTTP locally for scripts; HTTPS only for external.

**Files touched:**
- Modified: `Cargo.toml`, `src/main.rs`, `src/config.rs`, `README.md`.

**Verification:**
- Start with no cert paths → HTTP as before.
- Start with cert paths → HTTPS, `curl https://zun.tail...ts.net/api/v1/health` works with system trust store.

---

### Phase 8 — Passkey auth + sessions

**Goal:** replace the static bearer token with passkey-based login issuing opaque session tokens.

**Dependencies:** `webauthn-rs`, `sha2`, `rand` (already present).

**Tasks:**

1. Migrations: `passkeys`, `sessions`, `webauthn_challenges`, `enrollment_tokens`.
2. New module `src/auth/` with submodules:
   - `session.rs`: token generation (`zun_at_`, `zun_rt_` prefix + 32 bytes base64url), sha256 hashing, DB lookup middleware.
   - `passkey.rs`: WebAuthn register/authenticate ceremonies.
   - `enrollment.rs`: create/consume one-time enrollment tokens.
3. Rewrite the auth middleware (from Phase 1) to look up session rows instead of comparing a static token. `UserId` extension still attaches on success.
4. New auth endpoints (see API section). Implement each as thin wrappers around `webauthn-rs` calls + DB writes.
5. Admin CLI: `zun-admin user create <username>`, `zun-admin user delete <username>`, `zun-admin enroll <username>` (prints one-time URL).
6. Retire the `config.token` field. The first-time-setup flow becomes: `zun-admin user create` → `zun-admin enroll` → user opens URL, registers passkey, starts getting session tokens.
7. Android-side (not in this repo but coordinate): Credential Manager integration using the `androidx.credentials` library. Store session tokens in `EncryptedSharedPreferences`. Handle 401 → refresh → retry pattern.
8. Digital Asset Links: serve `/.well-known/assetlinks.json` matching the Android app's package signature. Document the SHA-256 fingerprint retrieval.

**Files touched:**
- New: `src/auth/` (replaces existing `src/auth.rs`), multiple migrations, Android `assetlinks.json` served from a static route.
- Modified: `src/main.rs`, `src/lib.rs` (router), `src/config.rs`.
- Removed: the single-token path in `config.toml` and `main.rs`.

**Verification:**
- End-to-end: admin creates user, prints enrollment URL, user (tester) registers passkey, receives session tokens, makes authenticated request.
- Expired access token returns 401 with `WWW-Authenticate: Bearer error="invalid_token"`.
- `/auth/refresh` with valid refresh token rotates and issues a new pair.
- `/auth/refresh` with already-rotated refresh token returns 401 (theft detection).
- Revoking a session via `/auth/sessions/{id}/revoke` invalidates it on the next request.
- `zun-admin user delete` cascades to passkeys, sessions, inputs, jobs, custom_prompts, and removes `data/users/<uid>/`.

---

## Coordination with project-zun

Phase 5 (seed injection) is the only cross-repo change. It must land in `project-zun` *before* the server-side phase 5 rolls out, since the server starts requiring a `SEED_PLACEHOLDER` in every sampler-bearing workflow.

**In project-zun:**

1. Update `project-zun/doc/WORKFLOWS.md`: add `SEED_PLACEHOLDER` to the list of placeholders for edit workflows. Mark it as required for any workflow containing a sampler node.
2. Edit each workflow JSON that has a `KSampler` / equivalent: replace the hardcoded `seed` integer with the string `"SEED_PLACEHOLDER"`.
3. Any tooling in project-zun that validates workflows needs to accept this.

**Order:**
- Land the project-zun change first.
- Refresh the `data/workflows/` symlink (no action — symlink already points at the updated files).
- Land server Phase 5. Server's existing placeholder tests will catch any missed workflow.

---

## Explicit non-goals

These are deliberately *not* built in v2. Documenting them so a future engineer doesn't assume "simple oversight."

- **User settings / preferences.** No `user_settings` table, no PATCH /me/settings. Clients manage their own preferences locally. Revisit in v3.
- **Favorites / tags / collections.** Possible one-boolean addition later; not in v2.
- **Live progress streaming.** Deferred — see `plan/COMMS_OPTIMIZATION.md#1`.
- **Multiple concurrent jobs.** FLUX saturates the GPU; serial is correct.
- **Batch grouping as a first-class API.** Android submits N times; gallery is ordered by `created_at`.
- **Cross-user input dedup.** Privacy: users do not share input cache.
- **Web frontend.** Schema and auth shapes are compatible (session cookies would be a trivial addition), but we don't ship a web UI in v2.
- **OAuth / passwords / magic links.** Passkeys only. Period.
- **Export / data dump endpoint.** User can back up the SQLite file and their `data/users/<uid>/` directory via SSH.

---

## Open decisions

Call these out to the human (the user) before shipping. They have reasonable defaults chosen but would benefit from explicit sign-off.

1. **Starter prompt seeding — automatic at user creation, or manual CLI only?**
   - Default assumption: manual (`zun-admin seed-prompts yanwen --from starter_prompts.toml`). User runs it once after creating themselves.
   - Alternative: `zun-admin user create` automatically seeds from a default starter file.
   - Trade: automation vs. explicitness. Either is fine.

2. **Input cache TTL.**
   - Default assumption: 30 days unused → file purged (row retained).
   - Configurable via `config.toml` (`input_cache_ttl_days`).
   - Trade: short TTL saves disk, long TTL saves re-uploads.

3. **Soft-delete grace period.**
   - Default assumption: 30 days → hard-deleted + files removed.
   - Configurable.
   - Trade: long period gives recovery confidence; short period reclaims disk.

4. **Android package signature for assetlinks.json.**
   - Needed in Phase 8. User must produce the SHA-256 of the app signing cert and paste into config or serve a signed assetlinks.json from the repo.
   - This is a human/app-side task; the server just needs the value.

5. **Single-user seed details.**
   - Phase 1 seeds user id=1 with username "admin" and display_name "admin". Is this OK, or does the user want a specific username from day one?
   - No real impact — it can be renamed later with `zun-admin user rename`.

6. **Do we want `POST /api/v1/jobs/{id}/regenerate` as explicit syntactic sugar?**
   - Semantically equivalent to: fetch job, grab `(input_sha256, prompt_id or prompt_text + workflow)`, submit again.
   - Default assumption: no. Client does it client-side; keeps the API smaller.
   - Alternative: yes, as a UX nicety — one-liner on Android.

7. **Should the "Try different prompt" flow get a dedicated endpoint?**
   - Same reasoning as above. Default: no, client composes it from existing endpoints.

---

## Quick reference for a coder agent

**If in doubt about user scoping:** every handler takes `Extension<UserId>`. Every SQL touching user data joins on or filters by `user_id`. If a helper function returns rows without a `UserId` parameter, it's wrong.

**If in doubt about paths:** use `paths::user_data_path(user_id, "cache/inputs" | "outputs" | "thumbs", filename)`. Never `format!("data/...")` directly.

**If in doubt about deletes:** soft delete first (set `deleted_at`). Hard delete is only in the purge task.

**If in doubt about tokens/secrets:** store sha256 hashes in DB. Plaintext never touches the database.

**If in doubt about seeds:** `rand::random::<i64>()` at submit time; persist on the row; pass to the worker; substitute into `SEED_PLACEHOLDER`.

**If in doubt about workflows:** workflow JSONs are opaque. Never parse node IDs. Only whole-string placeholder substitution.

**Run before committing:** `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

---

## Status

- Current state: v1 on `dev` branch, jobs + prompts.toml + single bearer token.
- v2 execution has not started. This doc is the spec.
- Expected timeline (rough): 6–8 focused working sessions, one per phase, independently committable.
