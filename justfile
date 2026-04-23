# zun-rust-server recipes. Run `just --list` to see them.
#
# First-time setup on a fresh clone:
#   just setup    # creates .env with a fresh token
#   just serve-dev
#
# See .env.example for the variables you can tune.

set dotenv-load := true

# Show available recipes.
default:
    @just --list

# Print the current ZUN_TOKEN from .env (full value). Handy when the
# Android coder needs to mirror it in their local.properties.
token:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f .env ]]; then
        echo "error: .env does not exist. run: just setup" >&2
        exit 1
    fi
    VAL=$(grep '^ZUN_TOKEN=' .env | cut -d= -f2- || true)
    if [[ -z "$VAL" ]]; then
        echo "error: ZUN_TOKEN= line in .env is empty" >&2
        exit 1
    fi
    echo "$VAL"

# First-time bootstrap: create .env with a fresh token, and copy the
# prompts template if data/prompts.yaml doesn't exist. Idempotent — a
# second `just setup` is a no-op on anything that already exists.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    # --- .env ---
    if [[ -f .env ]]; then
        echo ".env already exists — leaving it alone."
    elif [[ ! -f .env.example ]]; then
        echo "error: .env.example is missing from the repo root." >&2
        exit 1
    else
        TOKEN=$(openssl rand -hex 32)
        sed "s|^ZUN_TOKEN=$|ZUN_TOKEN=${TOKEN}|" .env.example > .env
        chmod 600 .env
        echo "wrote .env (mode 600) with a freshly generated 64-char token."
    fi
    # --- data/prompts.yaml ---
    if [[ -f data/prompts.yaml ]]; then
        echo "data/prompts.yaml already exists — leaving it alone."
    elif [[ ! -f data/prompts.example.yaml ]]; then
        echo "error: data/prompts.example.yaml missing; cannot bootstrap prompts." >&2
        exit 1
    else
        cp data/prompts.example.yaml data/prompts.yaml
        echo "wrote data/prompts.yaml from template — edit with your real prompts."
    fi
    echo "next: just serve-dev"

# Run in development mode: debug build, pretty logs, bound to 127.0.0.1.
serve-dev: _require-env
    #!/usr/bin/env bash
    set -euo pipefail
    export ZUN_LOG_FORMAT=pretty
    export ZUN_BIND=127.0.0.1:8080
    echo "dev  | bind=${ZUN_BIND} | data=./data | comfy=${ZUN_COMFY_URL:-http://127.0.0.1:8188}"
    cargo run

# Run in production mode: release build, JSON logs, bound to Tailscale IP.
serve-prod: _require-env
    #!/usr/bin/env bash
    set -euo pipefail
    TAILNET_IP=$(tailscale ip -4 2>/dev/null | head -1)
    if [[ -z "$TAILNET_IP" ]]; then
        echo "error: could not determine tailnet IP." >&2
        echo "       is tailscaled running? try: sudo tailscale up" >&2
        exit 1
    fi
    export ZUN_LOG_FORMAT=json
    export ZUN_BIND="${TAILNET_IP}:8080"
    echo "prod | bind=${ZUN_BIND} | data=./data | comfy=${ZUN_COMFY_URL:-http://127.0.0.1:8188}"
    cargo run --release

# Internal precondition: .env must exist and ZUN_TOKEN must be set.
# Run `just setup` to create .env from the template.
_require-env:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f .env ]] && [[ -z "${ZUN_TOKEN:-}" ]]; then
        echo "error: .env is missing and ZUN_TOKEN is not in your environment." >&2
        echo "       run:  just setup" >&2
        echo "       then: just serve-dev (or serve-prod)" >&2
        exit 1
    fi
    if [[ -z "${ZUN_TOKEN:-}" ]]; then
        echo "error: ZUN_TOKEN is not set." >&2
        echo "       check .env — the ZUN_TOKEN= line needs a value." >&2
        echo "       or regenerate: rm .env && just setup" >&2
        exit 1
    fi
    if [[ "${#ZUN_TOKEN}" -lt 16 ]]; then
        echo "error: ZUN_TOKEN must be at least 16 characters (got ${#ZUN_TOKEN})." >&2
        echo "       regenerate: rm .env && just setup" >&2
        exit 1
    fi
