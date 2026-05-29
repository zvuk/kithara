<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-test-utils.svg)](https://crates.io/crates/kithara-test-utils)
[![docs.rs](https://docs.rs/kithara-test-utils/badge.svg)](https://docs.rs/kithara-test-utils)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-utils

Cross-crate test-runtime support: `#[kithara::test]` macro re-exports, USDT probe helpers, hang-watchdog, and `unimock` glue. It carries the test runtime only — the integration-test fixtures themselves (synthetic servers, signal generators, builders) live in `kithara-integration-tests`. The probe/hang/mock paths are no-ops in release builds, so production code can depend on it normally.

## Modules

<table>
<tr><th>Module</th><th>Feature</th><th>Role</th></tr>
<tr><td><code>test</code></td><td>always on</td><td>Re-exports <code>kithara_test_macros::test</code>; <code>init_tracing</code>, <code>setup_tracing</code>, <code>setup_tracing_with_filter</code> helpers</td></tr>
<tr><td><code>hang</code></td><td><code>hang</code> (default)</td><td>Hang-watchdog primitives used by <code>#[kithara::test]</code>; <code>noop</code> fallback when the feature is off</td></tr>
<tr><td><code>probe</code></td><td><code>probe</code></td><td>USDT probe runtime helpers consumed by code annotated with <code>#[kithara::probe(...)]</code>; <code>noop</code> fallback when disabled</td></tr>
<tr><td><code>mock</code></td><td><code>mock</code></td><td><code>unimock</code> glue for trait-level mocks</td></tr>
<tr><td><code>kithara</code></td><td>always on</td><td>Re-exports macros from <code>kithara-test-macros</code> so consumers can write <code>#[kithara::test]</code>, <code>#[kithara::probe]</code>, <code>#[kithara::mock]</code>, <code>#[kithara::fixture]</code>, <code>#[kithara::hang_watchdog]</code>, and the <code>Probe</code> derive</td></tr>
</table>

## Usage

```rust
use kithara_test_utils::kithara;

#[kithara::test(tokio, timeout(std::time::Duration::from_secs(10)))]
async fn smoke() {
    // test body — protected by the hang watchdog (default-on)
}
```

For trait mocks:

```rust
use kithara_test_utils::kithara;

#[kithara::mock]
trait Service {
    fn get(&self) -> u64;
}
```

## Probe capture

The probe capture helper records every `tracing::event!` whose target ends with `_probe` (all `#[kithara::probe]` expansions emit to `<crate_name>_probe`, e.g. `kithara_stream_probe`). Events are kept in a process-wide `Vec` so a test can snapshot the full sequence and assert on it.

- **Why a tracing layer (not just `EventBus`)**: `kithara_events::EventBus` is a `tokio::sync::broadcast` — under load, lagged subscribers drop events. Probes fire at the decision site and the tracing layer records every emission without a bounded channel (defense in depth).
- **Why a process-wide subscriber**: `tracing::subscriber::set_default` is thread-local, but probes fire on the tokio worker threads spawned by `Downloader::run`, which don't inherit a per-test default. A single global subscriber is installed and captured events are filtered per-test by start timestamp. Because `#[kithara::test]` initialises a global subscriber via `setup_tracing_with_filter`, the probe layer must be attached inside that init path (`fixtures::init_tracing` installs it alongside the fmt layer) — a separate `set_global_default` would fail with `SetGlobalDefault`.
- Tests that capture probes should be `#[serial]` (via `serial_test`) so concurrent runs do not pollute the shared recorder.
- **Activation**: probe sites compile to no-ops unless `kithara-stream/usdt-probes` and/or `kithara-hls/usdt-probes` are enabled in the test build.

## Features

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>hang</code></td><td>yes</td><td>Real hang-watchdog implementation (otherwise no-op)</td></tr>
<tr><td><code>mock</code></td><td>no</td><td>Pulls <code>unimock</code> into the dependency graph; enables real <code>kithara::mock</code> expansion</td></tr>
<tr><td><code>probe</code></td><td>no</td><td>Pulls <code>usdt</code>; enables real USDT probe emission (otherwise no-op)</td></tr>
</table>

Consumer crates typically enable `mock` and `probe` in their `[dev-dependencies]` while keeping the default `hang` feature on.

## Integration tests live elsewhere

The integration-test domain (synthetic HLS servers, signal generators, `TestHttpServer`, `TestServerHelper`, `HlsFixtureBuilder`, `PackagedTestServer`, …) lives in `kithara-integration-tests` (`tests/`). To use it from another crate's tests, depend on `kithara-integration-tests` (it is `publish = false`) rather than re-implementing fixtures here.

See `tests/README.md` for the integration-test suite layout, the standalone `test_server` binary, the WASM flow, and the available fixture builders.

## Integration

Consumed by every crate's `[dev-dependencies]`. The macros it re-exports work on native and `wasm32` targets transparently.
