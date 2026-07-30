set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Type-check the workspace.
[default]
workspace:
    cargo check --workspace

clippy:
    cargo clippy --workspace -- -D warnings

[positional-arguments]
clippy-fix *ARGS:
    #!/bin/sh
    set -eu
    exec cargo clippy --fix --workspace --all-targets "$@" -- -D warnings

doc:
    cargo doc --workspace --no-deps --open
