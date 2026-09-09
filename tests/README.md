# Tests

Domain suites live in `tests/crates/<domain>/tests/`, with each package's
`Cargo.toml` beside that directory. Suite entrypoints select focused modules;
subdirectories group scenarios rather than repeat the package name.

`tests/crates/integration` owns cross-component scenarios (multi-instance,
phase continuity, thread budgets and integration regressions), shared fixture
helpers in `src/`, performance scenarios and benches. Its package name remains
`kithara-integration-tests`, which domain packages use for shared helpers.
`tests/crates/harness` owns fixture-artifact, browser-runner, blocking-detector,
flash and timeout tests. ABR contracts live in `abr`; platform loom models live
in `platform`. Fuzz targets remain in `tests/fuzz`.

 Binaries: `test_server` serves
`/assets/*` (checked-in regression files), `/signal/*` (procedural encoded audio)
and `/stream/*` (synthetic HLS); `wasm_test_runner` is the `wasm32` runner in
`.cargo/config.toml` that starts it.

Command surface and harness rules: `AGENTS.md`, `docs/guides/test-harness.md`.
This file carries what they cannot — which lane builds a suite.

## Which suite runs where

A suite behind `required-features` is invisible to `just test` **and**
`just test run <filter>`: the filter matches nothing, the target was never
built. Lanes are `just test run --lane=<name>`, from `[test.lanes.*]` in
`.config/xtask.toml`; the gating feature is in parentheses.

| Suite | Built by |
|---|---|
| `suite_light`, `suite_heavy`, `suite_stress` | `just test` |
| `suite_perf`, `memory_rss` (`perf`) | `just perf`; two `[[test]]` targets |
| `suite_harness` in `harness` (`harness`) | lane `fixtures` |
| `broadcast` in `broadcast` (`broadcast`) | lane `broadcast` |
| `e2e` in `play` (`e2e`) | lane `e2e`; needs a real output device |
| `network` in `play` and `queue` (`network`) | lane `network`; needs `KITHARA_DRM_PROD_*` |
| `network_manual` in `play` and `queue` (`network-manual`) | lane `network-manual`; corporate DNS + a device, so no CI runner |
| `suite_integration_regressions` | own lane; some tests are red on purpose |
| selenium tests (`selenium`) | lane `selenium-firefox` |
| `loom` models in `platform` | lane `loom`; `just test` builds the target, explores nothing |

`just test` also covers neither `kithara-ui`, nor `kithara-app` GUI tests, nor
this crate's own lib tests and the `flash` harness binaries: `default-filter` in
`.config/nextest.toml` removes them; `just test ui` and lane `harness` own them.

## Where a test runs: `#[kithara::test]`

`browser` and `wasm` run native and in the browser; `native`, plain `tokio` and
`selenium` are native-only. Modifiers: `serial` (exclusive), `multi_thread`
(multi-thread tokio instead of current-thread), `timeout(...)`. `selenium`
implies native + tokio + serial + multi_thread and auto-ignores the test.

`timeout(...)` is the real per-test bound — its watchdog aborts at budget+3s. The
default profile's 120s slow-timeout only backstops a test with no budget of its
own.

## Fixtures

Native tests take in-process URLs from `TestServerHelper` and `hls_server`:
`TestServer` (fixed 3-variant HLS), `HlsTestServer` (variants, segments, delays,
encryption, HEAD mismatch), `AbrTestServer` (bitrate switching). WASM tests hit
the same binary on `http://127.0.0.1:3444`, which `TEST_SERVER_URL` overrides.
Complex `/signal` and `/stream` specs register through `POST /token`; helpers
hand back ordinary `Url`s, so a test never sees the token.

`tests/crates/integration/src/fixture_protocol.rs` owns the synthetic-HLS wire types (`DataMode`,
`InitMode`, `DelayRule`, `EncryptionRequest`) and the deterministic byte oracles,
so byte assertions agree across helpers. Audio inputs, including small PCM
arrays and encoded `/signal/*` assets, are prepared by `kithara-test-fixtures`
at build time and injected through `#[kithara::fixture]` parameters. Encoding
under test remains part of the test action.

The build uses `kithara-encode` and system FFmpeg. Set `KITHARA_FIXTURE_CACHE`
before building to select the prepared-asset store; use an empty, separate
directory for a cold build. Runtime helpers serve the prepared bytes.

## WASM

`just test wasm [chrome|firefox|safari] [all|webcodecs]`. `just platform wasm` is
check/build/size-check only and runs no tests.

- A `wasm32` build makes no host binary and the runner looks for `test_server`
  beside itself, so the recipe builds it first.
- Only `suite_heavy` is built for `wasm32`, with every native module compiled
  out — `kithara_ffi_web` and `kithara_play::offline_browser` are the
  browser-visible coverage.
- The offline harness in `tests/crates/integration/src/offline` builds on both targets; only
  `app.rs`, which needs `kithara-app`, is gated to native.
- `OfflineWorker` owns the `OfflinePlayer`, on wasm from a Web Worker, because
  `Platform::offline` refuses a Host on the browser main thread. Open the
  resource inside a command, not on the driver.
- `webcodecs` belongs to `kithara-decode`, not any integration suite, and is
  Chromium-only.

### Selenium

Player scenarios drive the real page via thirtyfour:
`tests/crates/ffi-web/tests/selenium.rs`, auto-ignored by the macro flag.
Capabilities are in `tests/webdriver.json`;
`KITHARA_SELENIUM_PAGE_URL` and `KITHARA_SELENIUM_WEBDRIVER_URL` attach to an
already-running page or driver instead of starting one.

## Perf and benches

Perf scenarios are `#[ignore]`d. Criterion targets in `tests/crates/integration/benches` set
`harness = false` and are compiled only by `just perf bench`, which only builds
in its default mode. No test lane touches them, so a changed signature breaks
them silently.

Fuzzing: `fuzz/README.md`.

## Adding a test

- Name the module in its suite root (`tests/crates/integration/tests/suite_*.rs`,
  `tests/crates/integration/perf/suite_perf.rs`). A file nobody names compiles into nothing and
  passes silently. A perf file also needs `#![cfg(feature = "perf")]` and a
  `[[test]]` entry carrying `required-features = ["perf"]`.
- Pick the suite and `#[kithara::test]` flags from the contract under test,
  not from convenience: a feature gate takes that contract off the default run.
- Test the owner boundary. A fixture that quietly compensates for production
  behaviour pins the compensation, not the contract.

## No-SYNC audio safety

`just test` carries three independent guards for playback with SYNC disabled:
unity time-stretch transparency under bounded shared-worker load
(`no_sync_passthrough`), real MP3 and local HLS through the shared session graph
at 44.1/48 kHz with 1/2/4 decks (`no_sync_real_media`), and the render hot-path
budget at 128/256/512/1024 frames with 1/2/4 tracks. All three run
`CochleaReport` over the final PCM and fail on decoder errors, event loss,
underruns, silence, clipping, non-finite samples, or a p99 render cost above half
the audio period.

They do not prove device behaviour: the timing guard measures the player render
core, not physical xruns. PCM stays in memory, so no artifact I/O sits inside
their timeout, and the input is a deterministic sine prepared once through the
shared fixture cache plus checked-in MP3/HLS served locally.
`just test audio-artifacts /absolute/output` replays them through opt-in recorder
twins, keeping float WAV and JSON manifests for listening.

## Nextest profiles

`.config/nextest.toml`: `default` (all threads, 120s backstop), `fast` (skips
`suite_heavy`), `stress` (no thread cap, because a player must pass under
contention; failure bodies land in the JUnit, not the console), `ci` (one retry),
plus `cold`, `harness`, `support`, `perf`, `rtsan`.

`default-filter` intersects with a command-line filter; a lane-level `-E` would
union with it and silently widen the run. That is why suite exclusions live in
`default-filter`, never in a lane's arguments.

## Troubleshooting

- WASM cannot connect — port 3444 taken, or no host `test_server` was built.
- Encode timeouts in a cluster — cold fixture cache; the second run is honest.
