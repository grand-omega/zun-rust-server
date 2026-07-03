-- Real generation progress (0.0–1.0), parsed from ComfyUI ws progress
-- frames while the job is running. done => 1.0, queued => 0.0.
ALTER TABLE jobs ADD COLUMN progress REAL NOT NULL DEFAULT 0.0;
