# zun-rust-server recipes. Run `just --list` to see them.
#
# First time on a new box:
#   just setup        # generates config.toml with a fresh bearer token
#   just run          # starts the server (debug build)
#
# Token rotation:
#   rm config.toml && just setup
#   then paste the new token into the Android client + password manager.

# Show available recipes.
default:
    @just --list

# Run the server with ./config.toml (debug build for fast iteration; use
# `cargo run --release -- --config config.toml` on prod boxes).
run:
    cargo run -- --config config.toml

# Generate config.toml from config.example.toml with a fresh bearer
# token. Refuses to overwrite — delete config.toml first to regenerate.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f config.toml ]]; then
        echo "error: config.toml already exists. rm it first to regenerate." >&2
        exit 1
    fi
    TOKEN=$(openssl rand -hex 32)
    sed "s|REPLACE_ME_RUN_JUST_SETUP|${TOKEN}|" config.example.toml > config.toml
    chmod 600 config.toml
    echo
    echo "================================================================"
    echo "  bearer token (paste into Android client + password manager):"
    echo
    echo "    ${TOKEN}"
    echo
    echo "  wrote config.toml. on prod, swap the comment markers between"
    echo "  the DEV and PROD blocks before running."
    echo "================================================================"

# Print the token from config.toml — for re-pasting into the Android
# client without scrolling the file.
token:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f config.toml ]]; then
        echo "error: config.toml does not exist. run \`just setup\` first." >&2
        exit 1
    fi
    grep -m1 '^token' config.toml | cut -d'"' -f2

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
