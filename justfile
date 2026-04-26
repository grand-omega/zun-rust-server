# zun-rust-server recipes. Run `just --list` to see them.
#
# First-time setup:
#   cp config.example.toml config.toml   # then edit: set token, bind address
#   cargo run

# Show available recipes.
default:
    @just --list

# Print the current token from config.toml.
token:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f config.toml ]]; then
        echo "error: config.toml does not exist. copy from config.example.toml" >&2
        exit 1
    fi
    VAL=$(grep '^token' config.toml | cut -d'"' -f2 || true)
    if [[ -z "$VAL" ]]; then
        echo "error: token is empty in config.toml" >&2
        exit 1
    fi
    echo "$VAL"

# Bootstrap: copy config if it doesn't exist yet. v2 prompts live in the DB.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f config.toml ]]; then
        echo "config.toml already exists — leaving it alone."
    else
        cp config.example.toml config.toml
        chmod 600 config.toml
        echo "wrote config.toml — edit it: set token and bind address."
    fi
    echo "next: cargo run"
    echo "then: just seed-prompts"

# Seed the starter prompts into the default admin user.
seed-prompts:
    cargo run --bin zun-admin -- seed-prompts admin --from starter_prompts.toml
