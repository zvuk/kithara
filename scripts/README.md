<div align="center">
  <img src="../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/zvuk/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/zvuk/kithara)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Scripts inventory

This directory keeps CI and developer helper scripts that are not easily expressed with existing Cargo tools.

## Kept scripts (`scripts/ci`)

- `check-arch.sh`: project-specific architecture checks (shared type uniqueness, dependency direction, facade boundaries).
- `check-play-traits-unimock.sh`: verifies `kithara-play` trait coverage for unimock generation.
- `compare-perf.sh`: compares `perf-results.txt` against baseline with threshold gates.
- `quality-report.sh`: generates quality inventory markdown with optional gate thresholds.
- `rstest-audit.sh`: advisory report for test files that may benefit from `rstest`.
- `trait-mock-audit.sh`: advisory report for trait/mock coverage.
- `trait-mock-exceptions.sh`: enforces explicit exceptions for trait files without unimock.
- `test-compare-perf.sh`: self-test for `compare-perf.sh`.
- `wasm-slim-check.sh`: wasm size-budget gate (`wasm-slim`).
- `wasm-test.sh`: wasm integration test entrypoint with required env knobs.

## Removed wrappers

The following scripts were removed because they were shell pass-through wrappers over existing `just`/Cargo commands and added maintenance noise:

- `bench-test.sh` -> use `just bench-ci`
- `coverage.sh` -> use `just coverage`
- `lint.sh` -> use `just lint-full`
- `perf-test.sh` -> use `just perf-test`
- `test.sh` -> use `just test-all`

## Rust ecosystem replacements already used

- Formatting/linting: `cargo fmt`, `cargo clippy`
- Static rules: `ast-grep` via `just ast-grep-blocking` / `just ast-grep-advisory`
- Tests: `cargo nextest`, `cargo test --doc`
- Coverage: `cargo llvm-cov`
- Bench compare: `critcmp` (optional in `just bench-ci`)
- Dependency hygiene: `cargo machete` (see `just machete`)
- Hooks: `prek` (`prek install -f` in repo root)

## Notes

- `scripts/ci/*` remain the project-specific policy layer (architecture checks, wasm size/test gates, quality reports).
- CI configs in `.gitlab-ci.yml` and `.github/workflows/*.disabled` are kept aligned with the same script/`just` entrypoints.
