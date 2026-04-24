-- Pre-generated medium-resolution preview (~1280px JPEG) for fast 4G display.
-- Lives next to thumb_path; populated by the worker at job-done time.
ALTER TABLE jobs ADD COLUMN preview_path TEXT;
