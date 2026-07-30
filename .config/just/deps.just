set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

deny:
    cargo deny check

machete:
    cargo machete --skip-target-dir $(find crates -maxdepth 1 -mindepth 1 -not -name kithara-workspace-hack | sort) tests examples xtask

shear:
    cargo shear --deny-warnings

hack:
    cargo hack check --feature-powerset --no-dev-deps --depth 2 --workspace \
      --exclude kithara-play \
      --exclude kithara-app \
      --exclude kithara-fuzz

semver:
    cargo semver-checks check-release --workspace --exclude kithara-fuzz

geiger:
    cargo geiger --all-targets --all-dependencies --output-format Ascii

[positional-arguments]
manifest *ARGS:
    just _xtask manifest "$@"
