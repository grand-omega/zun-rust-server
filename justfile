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

# Bootstrap: copy config and prompts template if they don't exist yet.
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
    if [[ -f data/prompts.toml ]]; then
        echo "data/prompts.toml already exists — leaving it alone."
    elif [[ ! -f data/prompts.example.toml ]]; then
        echo "error: data/prompts.example.toml missing." >&2
        exit 1
    else
        cp data/prompts.example.toml data/prompts.toml
        echo "wrote data/prompts.toml — edit with your real prompts."
    fi
    echo "next: cargo run"
