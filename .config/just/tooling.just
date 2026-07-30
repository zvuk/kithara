set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Install tools used by health, audits, and diagnostics.
install:
    cargo install --locked ast-grep
    cargo install --locked cargo-deny
    cargo install --locked cargo-machete
    cargo install --locked cargo-shear
    cargo install --locked cargo-hack
    cargo install --locked cargo-semver-checks
    cargo install --locked cargo-public-api
    cargo install --locked cargo-modules
    cargo install --locked cargo-workspace-unused-pub
    cargo install --locked cargo-bloat
    cargo install --locked cargo-depgraph
    cargo install --locked cargo-llvm-cov
    cargo install --locked cargo-mutants
    cargo install --locked cargo-nextest
    cargo install --locked cargo-geiger
    cargo install --locked cargo-sort
    cargo install --locked taplo-cli
    cargo install --locked tidy-json
    cargo install --locked md-formatter
    cargo install --locked typos-cli
    cargo install --locked similarity-rs
    cargo install --locked --force lockbud
    @echo "==> tools installed; run 'just ci health' to verify"

# Run the cached xtask transport explicitly.
[positional-arguments]
xtask *ARGS:
    @exec just _xtask "$@"

# Force-refresh the cached xtask binary.
refresh:
    @exec just _xtask-refresh
