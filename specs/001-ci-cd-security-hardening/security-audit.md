# Security Audit: zun-rust-server

**Date**: 2026-07-05
**Scope**: `zun-rust-server` on `main`, per [spec.md](./spec.md) and [data-model.md](./data-model.md)

| ID | Severity | Area | Location | Description | Rationale | Recommended Fix | Status |
|---|---|---|---|---|---|---|---|
| SEC-001 | low | dependency | `Cargo.lock`: `paste 1.0.15`, `num-bigint 0.4.7` (both transitive via `image` → `ravif` → `rav1e`, the AVIF encoder chain used by `src/derived_images.rs`) | `cargo audit` reports 0 known CVEs but 2 non-vulnerability warnings: `paste` is flagged unmaintained (RUSTSEC-2024-0436), and the resolved `num-bigint 0.4.7` is a yanked version. | Neither has a matching security advisory. `paste` is a narrow, stable proc-macro unlikely to need active changes; the yank reason for `num-bigint 0.4.7` isn't security-related per the advisory DB. AVIF thumb/preview generation is a deliberate, recently-added feature (see git history "Add AVIF derived image negotiation"), so dropping the `image` crate's `avif` feature to shed these transitive deps would be a disproportionate fix for two non-exploitable warnings. | No immediate code change. Add `cargo audit` as a CI gate (see `ci-cd-evaluation.md`) so a future *actual* vulnerability is caught automatically; periodically re-check whether `rav1e`/`ravif` upstream drops these transitive deps on their own. | accepted-risk |
| SEC-002 | low | auth | `src/config.rs:64-71` (`Config::load` token validation) | Token validation only rejects the literal placeholder string and enforces a 16-char minimum length — no charset/entropy check. An operator who hand-edits `config.toml` outside `just setup` could set a low-entropy token (e.g. 16 repeated characters). | The supported path (`just setup`) generates a 256-bit random hex token via `openssl rand -hex 32` and writes `config.toml` with `chmod 600`, so this requires actively deviating from the documented workflow. Still the only backstop against a badly hand-edited config. | Raised the minimum to 32 chars (`src/config.rs`, `config.example.toml`), matching what `just setup` already generates. Regression tests: `config::tests::rejects_token_shorter_than_32_chars`, `accepts_token_at_32_chars`. | fixed |
| SEC-003 | low | auth | `src/auth.rs:38` (`token_eq`) | `subtle::ConstantTimeEq` on `&[u8]` short-circuits on length mismatch before the constant-time byte comparison — documented, intentional behavior of the `subtle` crate, not a bug in this code. | Token length isn't treated as secret here (fixed 64-hex-char value); revealing a length mismatch to an attacker who already knows nothing about the token's bytes gives no practical advantage in a single-token system behind a proxy. | None needed. Optional defense-in-depth: compare `sha256(presented)` vs `sha256(token)` to normalize length, purely for auditor comfort. | accepted-risk |
| SEC-004 | low | outbound-http | `src/comfy.rs:203-209` (`connect_ws`), invoked from `src/worker.rs:270` | No timeout wraps the WebSocket connect+upgrade call, unlike the HTTP client (60s) and health client (10s) — only the OS-level TCP connect timeout applies, which can be very long or effectively unbounded if the backend accepts the TCP connection but never completes the WS upgrade. | The worker processes exactly one job at a time, so a stuck `connect_ws` call stalls the *entire* queue until the process is restarted — not attacker-triggerable (`comfy_url` is operator config), but a real self-inflicted availability gap if the local pipeline process wedges. | Added a 10s `tokio::time::timeout` around the connect+upgrade (`WS_CONNECT_TIMEOUT`), matching the health-check client. Regression test: `comfy::tests::connect_ws_times_out_if_backend_never_completes_upgrade`. | fixed |
| SEC-005 | low | outbound-http | `src/comfy.rs`: `upload_image_once` (143-151), `submit_prompt_once` (172-180), `get_history_once` (212-219), `view_once` (266-277) | Response bodies are fully buffered (`.json()`/`.bytes()`) with no size cap; reqwest has no built-in max-response-size guard. | `comfy_url` is operator-trusted, not attacker input, so the realistic trigger is a backend bug (e.g. a corrupted/oversized response), not exploitation — but impact is a possible OOM/crash of the only process on the box. | Optional: stream `.bytes_stream()` with a running byte counter and bail past a sane ceiling (e.g. 200MB images, a few MB JSON). Bigger lift than other findings here — deferred this round; operator chose not to include it in this pass. | open |
| SEC-006 | low | file-path | `src/backup.rs:52-79` (`snapshot_once`) | Writes the daily DB backup directly to its final path via SQLite `VACUUM INTO`, instead of temp-sibling + rename like `paths::atomic_write`/`atomic_copy` elsewhere. A kill mid-write leaves a corrupt `.db` at the real filename for up to 24h. | Not path-traversal/auth — but breaks the codebase's own "atomic write, never half-written" invariant, and backups are the only recovery path on a single-instance deployment with no replica. | `VACUUM INTO` now targets a `paths::tmp_sibling` (made `pub` for this reuse), then `tokio::fs::rename`s into the final dated name. Regression tests: `backup::tests::snapshot_once_writes_valid_db_with_no_leftover_tmp_file`, `snapshot_once_overwrites_same_day_snapshot_via_rename`. | fixed |
| SEC-007 | low | file-path | `src/derived_images.rs:165-215` (`render_only`) | Hand-rolls its own temp-file + rename inside `spawn_blocking` instead of reusing `paths::atomic_write` — still atomic, but a second, duplicated implementation of the same invariant. | No security impact (write is still atomic, destination filename is `paths::data_path`-guarded); purely a consistency/maintainability observation from verifying the atomic-write pattern is applied uniformly. | Optional: factor a blocking-safe atomic-write helper into `paths.rs` and have `render_only` call it, so there's one implementation of the invariant. Deferred this round; operator chose not to include it in this pass. | open |
| SEC-008 | low | file-path | `src/purge.rs:155-187` (`stale_inputs`) vs. `src/handlers.rs:76-96` (`submit_job`/`resolve_input`) | Theoretical TOCTOU: an input can exist without a referencing `jobs` row for a moment between `resolve_input` returning and the job insert completing. `purge`'s `NOT EXISTS` guard alone wouldn't protect that window. | Neutralized in practice: `resolve_input` stamps `last_used_at = now` in the same request, and the purge cutoff (`cache_ttl_seconds`, default 30 days) is day-granularity — only becomes real if an operator configures retention down to seconds/minutes, well outside the intended daily-housekeeping design. | If sub-day retention is ever supported, move the `jobs` insert before/in the same transaction as the last-used-at stamp, or add a short grace floor independent of configured TTL. Not urgent given current defaults. | accepted-risk |
| SEC-009 | low | file-path | `tests/` (no `tests/backup.rs`, no `tests/purge.rs`; zero unit tests in `src/backup.rs`/`src/purge.rs`) | `purge::run` (stale job/input deletion, dry-run) and `backup::snapshot_once`/`prune_old` have no automated regression coverage, unlike the rest of the codebase's integration-test discipline. | Not itself a vulnerability, but means the SEC-006 atomic-write gap and purge's file/DB-sync logic could regress silently. | Added `tests/purge.rs` (hard-delete removes output+thumb+preview+AVIF siblings and the row; stale unreferenced input cleared; input referenced by an active job is not purged; dry-run touches neither disk nor DB) and inline tests in `src/backup.rs` (`prune_old_removes_only_db_files_older_than_keep_days`, plus the SEC-006 tests above). | fixed |
| SEC-010 | low | input-validation | `src/handlers.rs:166-183` (multipart image content-type handling) | Content-type/extension is taken entirely from the client-supplied multipart `Content-Type` header, not sniffed from bytes; a best-effort dimension-probe failure is silently swallowed, so arbitrary bytes can be stored/served labeled as `image/jpeg`/`image/png`. | In a multi-tenant system this is a content-type-confusion vector; here the only caller already holds the single bearer token and could only attack their own client. Not a privilege-boundary violation under this threat model. | Optional hardening: reject the upload outright if the dimension-probe/decode fails, instead of silently caching non-image bytes. Not required given the single-token trust model. | accepted-risk |
| SEC-011 | low | input-validation | `tests/submit.rs`, `tests/images.rs` (coverage gap) | No test exercises the 20MB body-size limit (`DefaultBodyLimit`) or feeds genuinely corrupt/truncated image bytes through the full `resolve_input` path — existing tests use tiny well-formed placeholder bytes. | Writing the "assert 413" regression test this recommended is what surfaced SEC-013 below — a real bug the earlier code-reading pass missed. This finding's own remediation paid for itself. | Added `tests/submit.rs::submit_over_max_upload_bytes_returns_413` and `submit_with_non_image_bytes_is_accepted_with_null_dimensions`. | fixed |
| SEC-012 | low | input-validation | `src/handlers.rs:315` (`unreachable!()`) | Currently unreachable by construction (guarded by an `if let` a few lines above), but a future refactor could make it reachable, turning a latent logic bug into a process abort (`SIGABRT`) instead of a graceful error. | Not attacker-reachable today. Flagging because "robust" was explicitly requested — a crash-on-refactor footgun is exactly the kind of latent robustness gap worth closing cheaply. | Replaced `unreachable!()` with `Err(AppError::Internal(...))`. **No regression test**: the branch is unreachable through any input this codebase can construct today (that's the point of the finding) — noted here rather than faking a test that can't actually exercise the line. | fixed |
| SEC-013 | medium | input-validation | `src/error.rs` (`From<MultipartError> for AppError`) | **Discovered while fixing SEC-011.** Every `MultipartError` — including a body-size-limit rejection, which `axum::extract::multipart::MultipartError::status()` correctly reports as 413 — was unconditionally converted to `AppError::BadRequest` (400). A client hitting the 20MB cap got a 400 "invalid multipart" instead of 413, giving it no reliable way to distinguish "too big, split the upload" from "malformed, don't retry as-is." | This is a real client-facing correctness bug (not attacker-facing — the caller already holds the token), caught only because SEC-011's new regression test asserted the specific status code instead of just "some 4xx." | Fixed: `From<MultipartError>` now checks `e.status() == PAYLOAD_TOO_LARGE` and maps to a new `AppError::PayloadTooLarge` (413) instead of collapsing into `BadRequest`. Regression test: `tests/submit.rs::submit_over_max_upload_bytes_returns_413` (initially failed with `400 != 413` against the pre-fix code, confirming the bug; passes after). | fixed |

## Summary

**13 findings** — 12 `low` severity, 1 `medium` (SEC-013, discovered
mid-fix). No `critical`/`high` findings, so per spec Story 3's acceptance
criteria nothing here was strictly mandatory to fix. The operator chose to
fix 6 of the original 12 findings plus the test additions; fixing SEC-011
surfaced SEC-013, which was fixed immediately since it's a real,
low-cost, high-confidence correctness bug.

- **Fixed this round**: SEC-002 (token entropy check), SEC-004
  (`connect_ws` timeout), SEC-006 (atomic backup write), SEC-009 and
  SEC-011 (regression tests for purge/backup and upload edge cases),
  SEC-012 (`unreachable!()` → graceful error), SEC-013 (413 vs 400 status
  code, found while fixing SEC-011)
- **Deferred, still open** (bigger lift, operator chose not to include
  this round): SEC-005 (streamed response size cap), SEC-007 (dedupe
  atomic-write helper)
- **No code change recommended**: SEC-001, SEC-003, SEC-008, SEC-010
  (`accepted-risk` — already justified inline above)

All fixes pass the full quality gate: `cargo fmt --all -- --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` (113 tests, 0
failures — up from 101 before this feature: +2 in `config.rs`, +1 in
`comfy.rs`, +3 in `backup.rs`, +4 in the new `tests/purge.rs`, +2 in
`tests/submit.rs` — see individual finding rows for exact test names).

## Areas Reviewed With No Issues Found

For transparency (spec SC-001 — no follow-up research needed to trust
this report), these were explicitly checked and found sound, so they are
not repeated as findings:

- **auth**: missing/malformed-header vs. wrong-token responses are
  uniform (no timing/enumeration difference); `Authorization` header is
  redacted from `tracing` spans; the token is never logged raw or via
  `Debug`; every route needing auth is wired through the single
  `route_layer` middleware (verified against the full route list;
  `tests/auth.rs` covers missing header, wrong token, missing `Bearer `
  prefix, and the public `/health` exception).
- **outbound-http**: TLS/certificate validation is not disabled anywhere
  (rustls throughout, no `danger_accept_invalid_certs`-style API used);
  retries are capped at 3 attempts with bounded exponential backoff and
  correctly idempotency-aware (`submit_prompt` only retries pure connect
  failures); per-job WS completion wait has an explicit, configurable
  timeout; `comfy_url` is operator config, never influenced by request
  data (no SSRF surface).
- **file-path**: the `..`/separator traversal guard in `paths::data_path`
  is applied at every place a filename reaches disk — call sites that
  skip it only ever join server-generated, already-validated relative
  paths, never raw attacker input; purge's 30-day-default grace period
  and status-gated job transitions rule out a delete-while-writing race.
- **input-validation**: a 20MB body-size limit is configured and actually
  enforced — bytes past the cap are rejected before reaching the handler
  (the *status code* returned for that rejection was wrong; see SEC-013,
  now fixed). Malformed/garbage image bytes cannot reach a panic — the
  only decode attempt on raw upload bytes is `.ok()`-guarded, and the one
  full `.decode()` call operates on ComfyUI's own output, not user input;
  `input_sha256` is strictly hex-validated and `job_id` is a
  server-generated UUID, so neither can be used to inject a path-traversal
  payload even before `data_path`'s own guard.
