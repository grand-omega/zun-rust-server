# zun-rust-server recipes. Run `just --list` to see them.

# Auto-load a .env file at the repo root (gitignored) so you can keep
# ZUN_TOKEN out of your shell profile. Example:
#
#   ZUN_TOKEN=$(openssl rand -hex 32)
#   ZUN_COMFY_URL=http://127.0.0.1:8188
#
set dotenv-load := true

# Show available recipes.
default:
    @just --list

# Run in development mode: debug build, pretty logs, bound to 127.0.0.1.
serve-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${ZUN_TOKEN:?ZUN_TOKEN not set — export it or put it in .env}"
    export ZUN_LOG_FORMAT=pretty
    export ZUN_BIND=127.0.0.1:8080
    echo "dev  | bind=${ZUN_BIND} | data=./data | comfy=${ZUN_COMFY_URL:-http://127.0.0.1:8188}"
    cargo run

# Run in production mode: release build, JSON logs, bound to Tailscale IP.
serve-prod:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${ZUN_TOKEN:?ZUN_TOKEN not set — export it or put it in .env}"
    TAILNET_IP=$(tailscale ip -4 2>/dev/null | head -1)
    if [[ -z "$TAILNET_IP" ]]; then
        echo "error: could not determine tailnet IP." >&2
        echo "        is tailscaled running? try: sudo tailscale up" >&2
        exit 1
    fi
    export ZUN_LOG_FORMAT=json
    export ZUN_BIND="${TAILNET_IP}:8080"
    echo "prod | bind=${ZUN_BIND} | data=./data | comfy=${ZUN_COMFY_URL:-http://127.0.0.1:8188}"
    cargo run --release
