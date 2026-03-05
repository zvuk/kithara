<div align="center">
  <img src="../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/zvuk/kithara/branch/main/graph/badge.svg)](https://codecov.io/gh/zvuk/kithara)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../LICENSE-MIT)

</div>

# Tests

`tests/` is a dedicated workspace crate (`kithara-integration-tests`) that hosts:

- integration tests for workspace crates;
- performance regression tests (`hotpath`, ignored by default);
- Criterion microbenchmarks;
- local test binaries/fixtures (fixture server, WASM test runner).

## Layout

- `tests/tests/` — integration tests grouped by crate (`kithara_hls`, `kithara_file`, `kithara_decode`, ...).
- `tests/perf/` — perf scenarios, each behind `feature = "perf"` and `#[ignore]`.
- `tests/benches/` — Criterion benchmarks.
- `tests/bin/` — helper binaries:
  - `fixture_server` — dynamic fixture server for HLS/ABR test sessions (native + WASM).
  - `wasm_test_runner` — custom WASM test runner that auto-starts fixture server.

## Cross-Platform Test Architecture

Integration tests run on both **native** and **WASM (browser)** targets. The `#[kithara::test]` macro controls where tests run via flags:

| Flag | Runs on native | Runs in browser (WASM) |
|------|:-:|:-:|
| `#[kithara::test(tokio, browser, ...)]` | yes | yes |
| `#[kithara::test(wasm, ...)]` | yes | yes |
| `#[kithara::test(native, ...)]` | yes | no |
| `#[kithara::test(tokio, ...)]` (no flag) | yes | no |

### Fixture Server

Tests that need an HTTP server (HLS playlists, segments, ABR) use **fixture servers** with a cross-platform interface:

- **Native**: in-process axum server (`TestServer`, `HlsTestServer`, `AbrTestServer`)
- **WASM**: external `fixture_server` binary via HTTP session API

Fixture types (`tests/tests/kithara_hls/fixture/`):

| Fixture | Purpose |
|---------|---------|
| `TestServer` | Fixed 3-variant HLS content |
| `HlsTestServer` | Configurable variants, segments, delays, encryption, HEAD mismatch |
| `AbrTestServer` | ABR bitrate switching scenarios |

The WASM path sends config to the fixture server via `POST /session/{type}`, receives a `base_url`, and uses it for all requests. Cleanup happens via `DELETE /session/{id}`.

### Session Protocol

Shared protocol types live in `kithara-test-utils/src/fixture_protocol.rs`:

- `HlsSessionConfig`, `AbrSessionConfig`, `FixedHlsSessionConfig`
- `DataMode` (TestPattern, SawWav, PerVariantPcm)
- `InitMode` (None, WavHeader)
- `DelayRule` — declarative delay rules replacing closure-based `segment_delay`
- `SessionResponse` (session_id, base_url, total_bytes, init_len)

Data generation functions (`generate_segment`, `expected_byte_at_test_pattern`, `create_pcm_segments`) are also in `fixture_protocol.rs` — shared between server and client for byte-level verification.

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

## WASM Tests

WASM tests run via `wasm-bindgen-test` in headless Chrome. The `wasm_test_runner` binary auto-starts the fixture server before delegating to `wasm-bindgen-test-runner`.

```bash
# Recommended entrypoint (handles everything)
bash scripts/ci/wasm-test.sh

# Manual run (wasm_test_runner auto-starts fixture server)
cargo +nightly test --target wasm32-unknown-unknown -p kithara-integration-tests
```

Test categories on WASM:

- **`kithara_wasm/`** — WASM player unit tests (AudioWorklet, threading)
- **`kithara_hls/`** — HLS integration tests with `browser` flag (fixture server)
- **`kithara_file/live_stress_real_mp3`** — live stream tests with `browser` flag

The fixture server is configured via `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
runner = ["cargo", "run", "--bin", "wasm_test_runner", "-p", "kithara-integration-tests", "--"]
```

### Selenium E2E (`thirtyfour`)

WASM player Selenium scenarios are implemented as ignored integration tests in:

- `tests/tests/kithara_wasm/selenium.rs`

Run them explicitly:

```bash
cargo test -p kithara-integration-tests --test integration selenium_hls_log_scenario -- --ignored --nocapture

cargo test -p kithara-integration-tests --test integration selenium_player_scenarios -- --ignored --nocapture

cargo test -p kithara-integration-tests --test integration selenium_diagnostic_suite -- --ignored --nocapture
```

Environment knobs:

- `KITHARA_SELENIUM_BROWSER=chrome|firefox` (default: `chrome`)
- `KITHARA_SELENIUM_HEADLESS=true|false` (default: `true`)
- `KITHARA_SELENIUM_TOOLCHAIN=nightly` (default: `nightly`)
- `KITHARA_SELENIUM_PAGE_URL=http://...` (use external trunk page instead of auto-start)
- `KITHARA_SELENIUM_WEBDRIVER_URL=http://...` (use external webdriver instead of auto-start)

WebDriver capabilities/profile defaults are versioned in:

- `tests/webdriver.json`

### agent-browser status

`vercel-labs/agent-browser` can be used for local exploratory browser debugging, but it is not part of the canonical test path in this project.

Reasons:

- current regression suite is Rust-native (`cargo test` + `thirtyfour`);
- CI and hooks are Rust-first and deterministic around WebDriver runs;
- adopting agent-browser as the main runner would add an extra Node.js daemon + Playwright stack in CI.

Decision: keep `thirtyfour` Selenium integration tests as the required path, use `agent-browser` only as optional local tooling.

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

## Adding New Tests

Integration tests:

1. Add module/file under `tests/tests/` (group by crate/domain).
2. Register module in `tests/tests/integration.rs` when needed.
3. Prefer deterministic fixtures and local servers over external network.
4. Use `#[kithara::test(tokio, browser, ...)]` for tests that need a server — they'll run on both native and WASM.
5. Use `#[kithara::test(wasm, ...)]` for pure logic tests that can run on WASM.
6. Use `#[kithara::test(native, ...)]` for tests that require filesystem or OS-specific features.

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
- WASM tests fail to connect:
  fixture server starts automatically via `wasm_test_runner`. Set `FIXTURE_SERVER_URL` to override the default `http://127.0.0.1:3333`. Check that port 3333 is available.
