set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

deny:
    cargo fetch --locked
    cargo deny fetch db || echo "RustSec refresh unavailable; validating the cached database" >&2
    cargo deny --offline check

machete:
    cargo machete --skip-target-dir $(find crates -maxdepth 1 -mindepth 1 -not -name kithara-workspace-hack | sort) tests xtask

shear:
    # Advisory: this source-only scan cannot see every cfg-, bench-, and
    # wasm-only dependency closure, so it reports findings without gating CI.
    cargo shear || true

hack:
    just _xtask powerset --no-dev-deps
    cargo check -p kithara-warp --lib --no-default-features
    cargo check -p kithara-warp --lib --no-default-features --features stretch-signalsmith
    cargo check -p kithara-warp --lib --no-default-features --features stretch-bungee
    cargo check -p kithara-warp --lib --no-default-features --features stretch-signalsmith,stretch-bungee

# Baseline is a revision here, not crates.io: the workspace is unpublished.
# Routed through xtask so the lane holds the shared target directory's job
# lease for the whole comparison, like every other build lane.
semver BASELINE="HEAD~1":
    just _xtask semver --baseline "{{ BASELINE }}" --baseline-feature backend-cpal

geiger:
    cargo geiger --all-targets --all-dependencies --output-format Ascii

[positional-arguments]
manifest *ARGS:
    just _xtask manifest "$@"
