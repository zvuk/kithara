set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Type-check the workspace.
[default]
workspace:
    cargo check --workspace

# The compiler cache and incremental compilation exclude each other, and a
# workstation and a CI job want opposite halves of that trade. `xtask` owns the
# choice; see `kithara-devtools::sccache::clippy_cleared_for`.
clippy:
    just _xtask clippy

[positional-arguments]
clippy-fix *ARGS:
    #!/bin/sh
    set -eu
    exec cargo clippy --fix --workspace --all-targets "$@" -- -D warnings

doc:
    cargo doc --workspace --no-deps --open
