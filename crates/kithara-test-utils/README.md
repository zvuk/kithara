<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-test-utils.svg)](https://crates.io/crates/kithara-test-utils)
[![docs.rs](https://docs.rs/kithara-test-utils/badge.svg)](https://docs.rs/kithara-test-utils)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-test-utils

Cross-crate test-runtime support: `#[kithara::test]` macro re-exports, USDT probe helpers, hang-watchdog, and `unimock` glue. It carries the test runtime only — the integration-test fixtures themselves (synthetic servers, builders) live in `kithara-integration-tests`, and the waveforms they serve come from `kithara-test-fixtures`. Probe/mock emissions are cfg-gated and hang/probe modules use feature-controlled no-op fallbacks, so production code can depend on it normally.

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

Probe sites compile to no-ops unless the emitting crate's `usdt` feature is
enabled. On macOS, `usdt` emits native DTrace probes; on other targets, it
emits the same probe records through `tracing`.

## Key Types

<table>

<tr><th>Module</th><th>Feature</th><th>Role</th></tr>

<tr><td><code>test</code></td><td>always on</td><td>Re-exports <code>kithara_test_macros::test</code>; <code>init_tracing</code>, <code>setup_tracing</code>, <code>setup_tracing_with_filter</code> helpers</td></tr>

<tr><td><code>hang</code></td><td><code>hang</code> (default)</td><td>Hang-watchdog primitives used by <code>#[kithara::test]</code>; <code>noop</code> fallback when the feature is off</td></tr>

<tr><td><code>probe</code></td><td><code>usdt</code></td><td>USDT runtime helpers consumed by code annotated with <code>#[kithara::probe(...)]</code>; <code>noop</code> fallback when disabled</td></tr>

<tr><td><code>mock</code></td><td><code>mock</code></td><td><code>unimock</code> glue for trait-level mocks</td></tr>

<tr><td><code>rtsan</code></td><td>always on</td><td>RealtimeSanitizer permit helper used by the RTSan macros</td></tr>

<tr><td><code>kithara_platform</code></td><td>always on</td><td>Re-export used by macro expansions that need flash control paths</td></tr>

<tr><td><code>kithara</code></td><td>always on</td><td>Re-exports macros from <code>kithara-test-macros</code> so consumers can write <code>#[kithara::test]</code>, <code>#[kithara::probe]</code>, <code>#[kithara::mock]</code>, <code>#[kithara::fixture]</code>, <code>#[kithara::flash]</code>, <code>#[kithara::hang_watchdog]</code>, <code>#[kithara::rtsan_allow_blocking]</code>, <code>#[kithara::rtsan_forbid_blocking]</code>, and the <code>Probe</code> derive</td></tr>

<tr><td><code>kithara_facade</code></td><td>always on</td><td>Facade-path flash macro re-export used by the public <code>kithara</code> crate</td></tr>

</table>

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>flash</code></td><td>no</td><td>Forward flash virtual-time support to <code>kithara-platform</code></td></tr>

<tr><td><code>hang</code></td><td>yes</td><td>Real hang-watchdog implementation (otherwise no-op)</td></tr>

<tr><td><code>mock</code></td><td>no</td><td>Pulls <code>unimock</code> into the dependency graph; enables real <code>kithara::mock</code> expansion</td></tr>

<tr><td><code>usdt</code></td><td>no</td><td>Enables USDT probe emission: native DTrace on macOS and <code>tracing</code> on other targets (otherwise no-op)</td></tr>

<tr><td><code>client-reqwest</code></td><td>no</td><td>Forward the reqwest HTTP backend through <code>kithara-events</code></td></tr>

<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the wreq HTTP backend through <code>kithara-events</code></td></tr>

<tr><td><code>tls-rustls</code></td><td>no</td><td>Forward rustls TLS selection through <code>kithara-events</code></td></tr>

<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection through <code>kithara-events</code></td></tr>

</table>

Consumer crates enable `usdt` only when they need real USDT emission.

## Integration

Consumed by every crate's `[dev-dependencies]`. The macros it re-exports work on native and `wasm32` targets transparently.

### Integration tests live elsewhere

The integration-test domain (synthetic HLS servers, `TestHttpServer`, `TestServerHelper`, `HlsFixtureBuilder`, `PackagedTestServer`, …) lives in `kithara-integration-tests` (`tests/`), over the waveforms and encoded assets `kithara-test-fixtures` produces. To use it from another crate's tests, depend on `kithara-integration-tests` (it is `publish = false`) rather than re-implementing fixtures here.

See `tests/README.md` for the integration-test suite layout, the standalone `test_server` binary, the WASM flow, and the available fixture builders.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-test-utils) for detailed contracts, invariants, and internals.
