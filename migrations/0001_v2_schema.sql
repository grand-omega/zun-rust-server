-- v2 schema. Single migration; v1 DB is throwable.
-- See plan/NEXT.md "Data model" for the rationale.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    disabled_at   INTEGER
) STRICT;

CREATE TABLE inputs (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    sha256          TEXT NOT NULL,
    path            TEXT,
    original_name   TEXT,
    content_type    TEXT,
    size_bytes      INTEGER,
    width           INTEGER,
    height          INTEGER,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER NOT NULL,
    UNIQUE (user_id, sha256)
) STRICT;
CREATE INDEX inputs_user_last_used ON inputs(user_id, last_used_at);

CREATE TABLE custom_prompts (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label           TEXT NOT NULL,
    description     TEXT,
    text            TEXT NOT NULL,
    workflow        TEXT NOT NULL,
    timeout_seconds INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deleted_at      INTEGER
) STRICT;
CREATE INDEX custom_prompts_user ON custom_prompts(user_id, deleted_at);

CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    input_id        INTEGER NOT NULL REFERENCES inputs(id),
    prompt_id       INTEGER REFERENCES custom_prompts(id),
    prompt_text     TEXT,
    workflow        TEXT NOT NULL,
    seed            INTEGER NOT NULL,
    status          TEXT NOT NULL,
    comfy_prompt_id TEXT,
    output_path     TEXT,
    thumb_path      TEXT,
    error_message   TEXT,
    width           INTEGER,
    height          INTEGER,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    deleted_at      INTEGER,
    -- Exactly one of (prompt_id, prompt_text) must be set.
    CHECK ((prompt_id IS NULL) <> (prompt_text IS NULL))
) STRICT;
CREATE INDEX jobs_user_status  ON jobs(user_id, status, created_at DESC);
CREATE INDEX jobs_user_active  ON jobs(user_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX jobs_input        ON jobs(input_id);
CREATE INDEX jobs_status_queue ON jobs(status, created_at) WHERE status IN ('queued', 'running');
