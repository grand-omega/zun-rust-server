CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,
    status          TEXT NOT NULL,
    prompt_id       TEXT NOT NULL,
    input_path      TEXT NOT NULL,
    output_path     TEXT,
    thumb_path      TEXT,
    comfy_prompt_id TEXT,
    error_message   TEXT,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    width           INTEGER,
    height          INTEGER
) STRICT;

CREATE INDEX idx_jobs_status         ON jobs(status);
CREATE INDEX idx_jobs_created        ON jobs(created_at DESC);
CREATE INDEX idx_jobs_status_created ON jobs(status, created_at DESC);
