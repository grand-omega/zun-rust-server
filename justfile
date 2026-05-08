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

# Pull the latest workflow JSONs from the authoring repo into vendored
# `workflows/`. This is the workflow author's update step — `cargo build`
# bakes whatever's in `workflows/` into the binary via include_dir!.
# Override the source with: `just sync-workflows /path/to/workflows`.
sync-workflows src="../zun-flux-pipeline/workflows":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -d "{{src}}" ]]; then
        echo "error: source dir {{src}} does not exist" >&2
        exit 1
    fi
    rm -f workflows/*.json workflows/MANIFEST.yaml
    cp "{{src}}"/*.json workflows/
    if [[ -f "{{src}}/MANIFEST.yaml" ]]; then
        cp "{{src}}/MANIFEST.yaml" workflows/
    fi
    echo "synced workflows from {{src}}; rebuild to pick them up."
