# Tests

`tests/` is a dedicated workspace crate (`kithara-integration-tests`) that hosts:

- integration tests for workspace crates;
- performance regression tests (`hotpath`, ignored by default);
- Criterion microbenchmarks;
- local test binaries/fixtures (for example, HLS fixture server).

## Layout

- `tests/tests/` — integration tests grouped by crate (`kithara_hls`, `kithara_file`, `kithara_decode`, ...).
- `tests/perf/` — perf scenarios, each behind `feature = "perf"` and `#[ignore]`.
- `tests/benches/` — Criterion benchmarks.
- `tests/bin/` — helper binaries (`hls_fixture_server`).

## Running Tests

```bash
# Workspace default
cargo nextest run --workspace
cargo test --doc --workspace

# Integration-test crate only
cargo nextest run -p kithara-integration-tests

# Specific integration test target / filter
cargo test -p kithara-integration-tests --test events
cargo test -p kithara-integration-tests --test integration kithara_hls::basic_playback
```

`just` shortcuts (from repo root):

```bash
just test
just test-doc
just test-all
```

## Performance Tests (`tests/perf`)

Performance tests use [hotpath-rs](https://github.com/pawurb/hotpath-rs), run only with `perf` feature, and are ignored by default.

```bash
# Run all perf tests
cargo test -p kithara-integration-tests --features perf --release -- --ignored --test-threads=1

# Run one suite
cargo test -p kithara-integration-tests --features perf --release --test resampler -- --ignored --nocapture
cargo test -p kithara-integration-tests --features perf --release --test decoder -- --ignored --nocapture
cargo test -p kithara-integration-tests --features perf --release --test pool -- --ignored --nocapture
cargo test -p kithara-integration-tests --features perf --release --test abr -- --ignored --nocapture
cargo test -p kithara-integration-tests --features perf --release --test storage -- --ignored --nocapture
```

Suites in this crate:

- `resampler.rs` — `perf_resampler_scenarios`
- `decoder.rs` — `perf_decoder_scenarios`
- `pool.rs` — `perf_pool_scenarios`
- `abr.rs` — `perf_abr_scenarios`
- `storage.rs` — `perf_storage_scenarios`

Local compare flow:

```bash
just perf-test
./scripts/ci/compare-perf.sh perf-results.txt saved-baseline.txt 10
```

## Benchmarks (`tests/benches`)

Criterion benchmark target:

- `refactor_hotpaths.rs`

Run manually:

```bash
cargo bench -p kithara-integration-tests --bench refactor_hotpaths
```

Or with project shortcuts:

```bash
just bench-build
RUN_BENCHMARKS=1 BENCH_CANDIDATE_NAME=local just bench-ci
```

## WASM Stress Tests

WASM tests live in `tests/tests/kithara_wasm/` and run via `wasm-bindgen-test` with a local HLS fixture server.

Recommended entrypoint:

```bash
bash scripts/ci/wasm-test.sh
```

This script builds and runs `hls_fixture_server`, then executes wasm32 tests with required env and timeout settings.

## Adding New Tests

Integration tests:

1. Add module/file under `tests/tests/` (group by crate/domain).
2. Register module in `tests/tests/integration.rs` when needed.
3. Prefer deterministic fixtures and local servers over external network.

Performance tests:

1. Add file in `tests/perf/`.
2. Gate file with `#![cfg(feature = "perf")]`.
3. Mark heavy tests as `#[test] #[ignore]`.
4. Register test target in `tests/Cargo.toml` (`[[test]] ... required-features = ["perf"]`).

Benchmarks:

1. Add benchmark file in `tests/benches/`.
2. Register `[[bench]]` target in `tests/Cargo.toml` (if new).

## Troubleshooting

- `hotpath not found`:
  run with `--features perf`.
- `test not found` for perf suites:
  include `--ignored`.
- noisy perf results:
  use `--release`, `--test-threads=1`, and run on an idle machine.
