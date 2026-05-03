-- Collapse the v2 multi-user-shaped schema into the actual single-user
-- data model. Existing runtime only ever authenticated as user_id=1, so
-- preserve that user's rows and drop the unused ownership layer.

PRAGMA foreign_keys = OFF;

DROP INDEX IF EXISTS inputs_user_last_used;
DROP INDEX IF EXISTS custom_prompts_user;
DROP INDEX IF EXISTS jobs_user_status;
DROP INDEX IF EXISTS jobs_user_active;
DROP INDEX IF EXISTS jobs_input;
DROP INDEX IF EXISTS jobs_status_queue;

CREATE TABLE inputs_new (
    id              INTEGER PRIMARY KEY,
    sha256          TEXT NOT NULL,
    path            TEXT,
    original_name   TEXT,
    content_type    TEXT,
    size_bytes      INTEGER,
    width           INTEGER,
    height          INTEGER,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER NOT NULL,
    UNIQUE (sha256)
) STRICT;

INSERT INTO inputs_new
    (id, sha256, path, original_name, content_type, size_bytes, width, height, created_at, last_used_at)
SELECT id, sha256, path, original_name, content_type, size_bytes, width, height, created_at, last_used_at
FROM inputs
WHERE user_id = 1;

CREATE TABLE custom_prompts_new (
    id              INTEGER PRIMARY KEY,
    label           TEXT NOT NULL,
    description     TEXT,
    text            TEXT NOT NULL,
    workflow        TEXT NOT NULL,
    timeout_seconds INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deleted_at      INTEGER
) STRICT;

INSERT INTO custom_prompts_new
    (id, label, description, text, workflow, timeout_seconds, created_at, updated_at, deleted_at)
SELECT id, label, description, text, workflow, timeout_seconds, created_at, updated_at, deleted_at
FROM custom_prompts
WHERE user_id = 1;

CREATE TABLE jobs_new (
    id              TEXT PRIMARY KEY,
    input_id        INTEGER NOT NULL REFERENCES inputs_new(id),
    source_prompt_id INTEGER REFERENCES custom_prompts_new(id),
    prompt_text     TEXT NOT NULL,
    workflow        TEXT NOT NULL,
    timeout_seconds INTEGER,
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
    preview_path    TEXT
) STRICT;

INSERT INTO jobs_new
    (id, input_id, source_prompt_id, prompt_text, workflow, timeout_seconds, seed, status, comfy_prompt_id,
     output_path, thumb_path, error_message, width, height, created_at, started_at,
     completed_at, deleted_at, preview_path)
SELECT j.id, j.input_id, j.prompt_id, COALESCE(j.prompt_text, cp.text), j.workflow,
       COALESCE(cp.timeout_seconds, 60), j.seed, j.status, j.comfy_prompt_id,
       j.output_path, j.thumb_path, j.error_message, j.width, j.height, j.created_at, j.started_at,
       j.completed_at, j.deleted_at, j.preview_path
FROM jobs j
LEFT JOIN custom_prompts cp ON cp.id = j.prompt_id
WHERE j.user_id = 1;

DROP TABLE jobs;
DROP TABLE custom_prompts;
DROP TABLE inputs;
DROP TABLE users;

ALTER TABLE inputs_new RENAME TO inputs;
ALTER TABLE custom_prompts_new RENAME TO custom_prompts;
ALTER TABLE jobs_new RENAME TO jobs;

CREATE INDEX inputs_last_used ON inputs(last_used_at);
CREATE INDEX custom_prompts_deleted ON custom_prompts(deleted_at);
CREATE INDEX jobs_status       ON jobs(status, created_at DESC);
CREATE INDEX jobs_active       ON jobs(created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX jobs_input        ON jobs(input_id);
CREATE INDEX jobs_status_queue ON jobs(status, created_at) WHERE status IN ('queued', 'running');

PRAGMA foreign_keys = ON;
