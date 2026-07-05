#!/usr/bin/env bash
# Reads a list of paths on stdin (one per line) and fails if any of them
# looks like user data or a secret that must never reach the remote:
# images, logs, jobs.db, real config.toml/.env, or anything under data/.
# Shared by .githooks/pre-commit, .githooks/pre-push, and CI.
set -euo pipefail

PATTERN='^data/|(^|/)config\.toml(\..+)?$|\.(png|jpe?g|webp|avif|gif|bmp|tiff?)$|\.log$|(^|/)\.env$|\.db$|(^|/)\.claude/'

hits=$(grep -Ei "$PATTERN" || true)

if [[ -n "$hits" ]]; then
    echo "error: blocked — these paths look like user data or secrets and must never be committed:" >&2
    echo "$hits" | sed 's/^/  /' >&2
    echo >&2
    echo "If this is a false positive, adjust the pattern in scripts/check-forbidden-paths.sh." >&2
    exit 1
fi
