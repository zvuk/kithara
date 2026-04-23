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
- RSS memory profiling tests;
- Criterion microbenchmarks;
- fuzz targets (`cargo-fuzz`);
- local test binaries/fixtures (unified test server, WASM test runner).

## Layout

- `tests/tests/` — integration tests grouped by crate (`kithara_hls`, `kithara_file`, `kithara_decode`, ...).
- `tests/perf/` — perf scenarios, each behind `feature = "perf"` and `#[ignore]`.
- `tests/benches/` — Criterion benchmarks.
- `tests/fuzz/` — `cargo-fuzz` fuzz targets.
- `tests/bin/` — helper binaries:
  - `test_server` — unified synthetic/static media server exposing `/assets/*`, `/signal/*`, and `/stream/*`.
  - `wasm_test_runner` — custom WASM test runner that auto-starts `test_server`.

## Cross-Platform Test Architecture

Integration tests run on both **native** and **WASM (browser)** targets. The `#[kithara::test]` macro controls where tests run via flags:

| Flag | Runs on native | Runs in browser (WASM) |
|------|:-:|:-:|
| `#[kithara::test(tokio, browser, ...)]` | yes | yes |
| `#[kithara::test(wasm, ...)]` | yes | yes |
| `#[kithara::test(native, ...)]` | yes | no |
| `#[kithara::test(tokio, ...)]` (no flag) | yes | no |
| `#[kithara::test(selenium, ...)]` | yes | no |

Additional flags:

| Flag | Effect |
|------|--------|
| `serial` | Run test exclusively (no parallel tests) |
| `timeout(Duration::from_secs(N))` | Per-test timeout |
| `env("KEY", "VAL")` | Set env var for test duration |
| `soft_fail("pattern")` | Allow panics matching pattern |
| `multi_thread` | Use multi-thread tokio runtime instead of current-thread |
| `selenium` | Implies `native + tokio + serial + multi_thread`, adds `#[ignore = "requires selenium"]` |

### Unified Test Server

Tests that need HTTP media fixtures use the unified `test_server` with a cross-platform interface:

- **Native**: in-process helper-backed URLs via `kithara-test-utils::TestServerHelper` and `kithara-test-utils::hls_fixture::*`
- **WASM**: the same `test_server` binary on `http://127.0.0.1:3444`

The helper contract is token-backed but transparent to tests:

- complex `/signal/*` and `/stream/*` specs are registered through `POST /token`
- the server returns a UUID
- helper APIs return ordinary `Url`s that already point at `/signal/{token}...` or `/stream/{token}...`

Canonical fixture types (`kithara_test_utils::hls_fixture`):

| Fixture | Purpose |
|---------|---------|
| `TestServer` | Fixed 3-variant HLS content |
| `HlsTestServer` | Configurable variants, segments, delays, encryption, HEAD mismatch |
| `AbrTestServer` | ABR bitrate switching scenarios |

Synthetic HLS lives under `/stream/*`; procedural encoded audio lives under `/signal/*` (`sawtooth`, `sawtooth-desc`, `sine`, `sweep`, `silence`); repository-owned regression assets remain under `/assets/*`.

### Synthetic HLS Protocol

Shared synthetic HLS types live in `kithara-test-utils/src/fixture_protocol.rs`:

- `DataMode` (TestPattern, SawWav, PerVariantPcm, CustomData, blob-backed payloads)
- `InitMode` (None, WavHeader, Custom, blob-backed init payloads)
- `DelayRule` - declarative delay rules for synthetic segment serving
- `EncryptionRequest` - AES-128 parameters for encrypted fixture scenarios

Pure generation helpers such as `generate_segment`, `expected_byte_at_test_pattern`, and `create_wav_init_header` also live in `fixture_protocol.rs` so byte-level assertions stay deterministic across test helpers.

## Agent guardrails

- Start with the smallest deterministic repro and contract test. Escalate to heavy, live, or stress coverage only after the owned contract is captured.
- Tests should validate the owner boundary of the runtime or API. Do not make fixtures or harnesses silently compensate for production behavior.
- Choose the suite and `#[kithara::test]` flags from the contract you are exercising, not from local convenience.

## Running Tests

### Native prerequisites

Some native test fixtures now generate encoded audio via `ffmpeg-next` in `kithara-test-utils`.
That path requires a system FFmpeg installation plus `pkg-config`/`pkgconf` so Rust build scripts can find the FFmpeg libraries during compilation.

On macOS with Homebrew:

```bash
brew install pkgconf ffmpeg
```

If these tools are missing, native test builds that touch encoded `/signal/...{mp3,flac,aac,m4a}` fixtures will fail while building `ffmpeg-sys-next`.

```bash
# Workspace default
cargo nextest run --workspace
cargo test --doc --workspace

# Integration-test crate only
cargo nextest run -p kithara-integration-tests

# Specific integration test target / filter
cargo test -p kithara-integration-tests --test suite_light events::
cargo test -p kithara-integration-tests --test suite_heavy live_stress_real_stream::
```

`just` shortcuts (from repo root):

```bash
just test          # default nextest run
just test-fast     # fast profile: skips suite_heavy (stress/selenium)
just test-stress   # stress profile: only suite_heavy, 1 thread, 60s timeout
just test-doc      # doc tests only
just test-all      # both unit and doc tests
```

## WASM Tests

WASM tests run via `wasm-bindgen-test` in headless Chrome. The `wasm_test_runner` binary auto-starts `test_server` before delegating to `wasm-bindgen-test-runner`.

```bash
# Recommended entrypoint (handles everything)
just wasm-test

# Manual run (wasm_test_runner auto-starts test_server)
cargo +nightly test --target wasm32-unknown-unknown -p kithara-integration-tests
```

Test categories on `cargo +nightly test --target wasm32-unknown-unknown`:

- **`kithara_hls/`** — HLS integration tests with `browser` flag (unified `test_server`)

`tests/tests/kithara_wasm/stress.rs` currently contains ignored regression specs:
`Audio::new` stalls in the `wasm-bindgen-test` headless runner during bootstrap.
`tests/tests/kithara_file/live_stress_real_mp3.rs` is also ignored on `wasm32`
for the same reason.
Active browser/player coverage lives in the Selenium suite below.

The fixture server is configured via `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
runner = ["cargo", "run", "--bin", "wasm_test_runner", "-p", "kithara-integration-tests", "--"]
```

### Selenium E2E (`thirtyfour`)

Exported `kithara-wasm` player scenarios run through the real `kithara-wasm.js`
page and are implemented as ignored integration tests in:

- `tests/tests/kithara_wasm/selenium.rs`

Tests use the `#[kithara::test(selenium, ...)]` macro flag, which implies
`native + tokio + serial + multi_thread` and adds `#[ignore = "requires selenium"]`.

Run them explicitly:

```bash
# Single test
cargo test -p kithara-integration-tests --test suite_heavy selenium_player_scenarios -- --ignored --nocapture

# All selenium tests
cargo test -p kithara-integration-tests --test suite_heavy selenium -- --ignored --nocapture
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
cargo test -p kithara-integration-tests --features perf --release --test suite_perf -- --ignored --nocapture
```

Perf modules in this crate:

- `abr.rs` — `perf_abr_scenarios`
- `decoder.rs` — `perf_decoder_scenarios`
- `memory_rss.rs` — RSS memory profiling for HLS playback
- `pool.rs` — `perf_pool_scenarios`
- `resampler.rs` — `perf_resampler_scenarios`
- `storage.rs` — `perf_storage_scenarios`

Local compare flow:

```bash
just perf-test
cargo xtask perf-compare perf-results.txt saved-baseline.txt --threshold 10
```

## Benchmarks (`tests/benches`)

Criterion benchmark targets:

- `abr_estimator.rs` — ABR throughput estimation
- `bufpool.rs` — buffer pool allocation hot paths
- `refactor_hotpaths.rs` — general hot path benchmarks

Run manually:

```bash
cargo bench -p kithara-integration-tests --bench bufpool
cargo bench -p kithara-integration-tests --bench refactor_hotpaths
```

Or with project shortcuts:

```bash
just bench-build
RUN_BENCHMARKS=1 BENCH_CANDIDATE_NAME=local just bench-ci
```

Note: `abr_estimator` is in the `kithara-abr` crate, not `kithara-integration-tests`.

## Fuzzing (`tests/fuzz`)

Fuzz targets use `cargo-fuzz` / `libfuzzer-sys`:

- `aes_decrypt` — AES-128-CBC decryption with random key/iv/ciphertext
- `hls_parsing` — HLS M3U8 playlist parsing with `arbitrary`-generated inputs

Run:

```bash
# Install cargo-fuzz (once)
cargo install cargo-fuzz

# Run a fuzz target (requires nightly)
cd tests/fuzz
cargo +nightly fuzz run aes_decrypt -- -max_total_time=60
cargo +nightly fuzz run hls_parsing -- -max_total_time=60
```

## Adding New Tests

Integration tests:

1. Add module/file under `tests/tests/` (group by crate/domain).
2. Register light tests in `tests/tests/suite_light.rs`.
3. Register heavy or browser-integration tests in `tests/tests/suite_heavy.rs`.
4. Register perf-only tests in `tests/perf/suite_perf.rs`.
5. Prefer deterministic fixtures and local servers over external network.
6. Use `#[kithara::test(tokio, browser, ...)]` for tests that need a server — they'll run on both native and WASM.
7. Use `#[kithara::test(wasm, ...)]` for pure logic tests that can run on WASM.
8. Use `#[kithara::test(native, ...)]` for tests that require filesystem or OS-specific features.
9. Use `#[kithara::test(selenium, ...)]` for Selenium E2E tests (auto-ignored, multi-thread runtime).
10. Use `#[case::name(value)]` for parameterized test cases.

Performance tests:

1. Add file in `tests/perf/`.
2. Gate file with `#![cfg(feature = "perf")]`.
3. Mark heavy tests as `#[test] #[ignore]`.
4. Register test target in `tests/Cargo.toml` (`[[test]] ... required-features = ["perf"]`).

Benchmarks:

1. Add benchmark file in `tests/benches/`.
2. Register `[[bench]]` target in `tests/Cargo.toml` (if new).

Fuzz targets:

1. Add fuzz target in `tests/fuzz/fuzz_targets/`.
2. Register `[[bin]]` target in `tests/fuzz/Cargo.toml`.
3. Use workspace dependencies from root `Cargo.toml`.

## Nextest Profiles

| Profile | Command | Description |
|---------|---------|-------------|
| `default` | `just test` | All tests, 4 threads |
| `fast` | `just test-fast` | Skips `suite_heavy` (stress/selenium) |
| `stress` | `just test-stress` | Only `suite_heavy`, 1 thread, 60s slow-timeout |
| `ci` | `just test-ci` | CI mode, no fast-fail |

Profiles are defined in `.config/nextest.toml`.

## Troubleshooting

- `hotpath not found`:
  run with `--features perf`.
- `test not found` for perf suites:
  include `--ignored`.
- noisy perf results:
  use `--release`, `--test-threads=1`, and run on an idle machine.
- WASM tests fail to connect:
  `test_server` starts automatically via `wasm_test_runner`. Set `TEST_SERVER_URL` to override the default `http://127.0.0.1:3444`. Check that port 3444 is available.
