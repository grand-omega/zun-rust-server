# Phase 1 Data Model: CI/CD Evaluation & Security Hardening

This feature's "data" is the structure of its two report documents
(`security-audit.md`, `ci-cd-evaluation.md`), not application data. Both
entities below come from the spec's Key Entities section.

## Finding

One row per security-audit result in `security-audit.md`.

| Field | Type | Notes |
|---|---|---|
| `id` | string | `SEC-001`, `SEC-002`, ... sequential |
| `severity` | enum | `critical` \| `high` \| `medium` \| `low` |
| `area` | enum | `auth` \| `input-validation` \| `dependency` \| `file-path` \| `outbound-http` |
| `location` | string | file:line or component name (e.g. `src/auth.rs:42`, `Cargo.lock: paste 1.0.15`) |
| `description` | string | what the issue is, in plain terms |
| `rationale` | string | why it matters (or doesn't) under this system's actual single-user/single-token threat model (constitution Principle IV) |
| `recommended_fix` | string | concrete remediation step |
| `status` | enum | `open` \| `fixed` \| `accepted-risk` |

**Validation rules** (from spec FR-002, FR-006):
- Every finding MUST have all fields populated — no bare "consider
  hardening X" without a location and recommended fix.
- A finding MUST NOT recommend a fix that reintroduces multi-tenant
  defense-in-depth (per-IP rate limiting, tenant isolation, IP allowlists)
  unless `rationale` explicitly argues the Principle IV boundary itself
  (proxy + one bearer token) is inadequate.
- `status: accepted-risk` MUST include the reason in `rationale` (e.g. "no
  upstream fix available for this advisory").
- Only findings with `status: fixed` may reference a test file/name as
  evidence (added in Story 3).

## CI Gate

One row per pipeline step (existing or recommended) in `ci-cd-evaluation.md`.

| Field | Type | Notes |
|---|---|---|
| `name` | string | e.g. "forbidden-path check", "cargo audit" |
| `status` | enum | `existing` \| `recommended` |
| `catches` | string | class of defect this step catches |
| `does_not_catch` | string | explicitly named gap, if any |
| `cost` | string | added CI minutes and/or new external Action dependency |
| `decision` | enum | only for `recommended` rows: `adopt` \| `defer` (operator decides; default proposal is documented, not auto-applied) |

**Validation rules** (from spec FR-004):
- Every `recommended` gate MUST state `cost` — no free-form "just add X"
  without the tradeoff.
- No gate may be recommended solely because "more scanning is generally
  good" — it must trace back to a specific Finding it would have caught
  (per research.md R1) or a specific gap named in `does_not_catch` for an
  existing gate.
