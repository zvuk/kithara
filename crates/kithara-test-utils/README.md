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
    // An explicit timeout also applies with default features disabled.
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

<tr><td><code>test</code></td><td><code>test</code>; tracing also needs <code>subscriber</code> on <code>wasm32</code></td><td>Test tracing, clock measurement and USDT subscriber helpers</td></tr>

<tr><td><code>hang</code></td><td><code>hang</code> (default)</td><td>Hang-watchdog primitives used by <code>#[kithara::test]</code>; <code>noop</code> fallback when the feature is off</td></tr>

<tr><td><code>probe</code></td><td><code>usdt</code></td><td>USDT runtime helpers consumed by code annotated with <code>#[kithara::probe(...)]</code>; <code>noop</code> fallback when disabled</td></tr>

<tr><td><code>mock</code></td><td><code>mock</code></td><td><code>unimock</code> glue for trait-level mocks</td></tr>

<tr><td><code>rtsan</code></td><td>always on</td><td>RealtimeSanitizer permit helper used by the RTSan macros</td></tr>

<tr><td><code>kithara_platform</code></td><td>always on</td><td>Re-export used by macro expansions that need flash control paths</td></tr>

<tr><td><code>kithara</code></td><td>instrumentation always; test DSL with <code>test</code></td><td>Re-exports macros from <code>kithara-test-macros</code> so consumers can write <code>#[kithara::test]</code>, <code>#[kithara::probe]</code>, <code>#[kithara::mock]</code>, <code>#[kithara::fixture]</code>, <code>#[kithara::flash]</code>, <code>#[kithara::hang_watchdog]</code>, <code>#[kithara::rtsan_allow_blocking]</code>, <code>#[kithara::rtsan_forbid_blocking]</code>, and the <code>Probe</code> derive</td></tr>

</table>

## Features

<table>

<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>

<tr><td><code>test</code></td><td>yes</td><td>Test, fixture and asset macros, test runtime helpers and cancel-token fixtures; also enabled by <code>mock</code>, <code>http-server</code> and <code>temp-dir</code></td></tr>

<tr><td><code>flash</code></td><td>no</td><td>Forward flash virtual-time support to <code>kithara-platform</code></td></tr>

<tr><td><code>hang</code></td><td>yes</td><td>Real hang-watchdog implementation (otherwise no-op)</td></tr>

<tr><td><code>http-server</code></td><td>no</td><td>Local axum HTTP server for tests (native only)</td></tr>

<tr><td><code>temp-dir</code></td><td>no</td><td>Temporary-directory fixtures</td></tr>

<tr><td><code>load</code></td><td>no</td><td>One spinning thread per CPU during every <code>#[kithara::test]</code> body; enabled by <code>just test run --load=on</code></td></tr>

<tr><td><code>no-block</code></td><td>no</td><td>Forward the blocking-call detector to <code>kithara-platform</code></td></tr>

<tr><td><code>mock</code></td><td>no</td><td>Pulls <code>unimock</code> into the dependency graph; enables real <code>kithara::mock</code> expansion</td></tr>

<tr><td><code>subscriber</code></td><td>no</td><td>The test log subscriber on <code>wasm32</code>, where <code>setup_tracing</code> and its siblings exist only with it; native builds always carry it</td></tr>

<tr><td><code>usdt</code></td><td>no</td><td>Enables USDT probe emission: native DTrace on macOS and <code>tracing</code> on other targets (otherwise no-op)</td></tr>

<tr><td><code>client-reqwest</code>, <code>client-wreq</code>, <code>tls-rustls</code>, <code>tls-native</code></td><td>no</td><td>Empty compatibility features; select no HTTP or TLS dependencies</td></tr>

</table>

Consumer crates enable `usdt` only when they need real USDT emission.

With defaults disabled, instrumentation remains available without `test`; the
test, fixture and asset macro entrypoints are absent. Test-utils defaults
enable `hang` and `test`. The public `kithara` facade has its own `test`
feature for its test and fixture exports.

## Integration

Consumed by workspace `[dev-dependencies]` and independent native/wasm test suites.

### Independent consumers

The minimal native test consumer needs only `kithara-test-utils` with default
features disabled and `test` enabled. Plain tests, `#[case]` parameterization, fixture injection, `serial`, and
current-thread async tests use its re-exported runtime. Generated serialization
wrappers require no direct `serial_test` dependency.

```toml
[dev-dependencies]
kithara-test-utils = { git = "https://github.com/zvuk/kithara", rev = "<tested-commit>", default-features = false, features = ["test"] }
```

Use the same tested commit for all Kithara dependencies. Optional capabilities
have these additional requirements:

| Capability | Consumer requirements |
| --- | --- |
| Trait mocks | Enable test-utils `mock`; add direct `unimock = { version = "0.6", features = ["std"] }` because generated attributes refer to `::unimock` |
| Native `#[kithara::test(tokio, multi_thread)]` | Add direct `kithara-platform` with `tokio-rt-multi-thread` enabled |
| Hang watchdog | Enable test-utils `hang`; use `#[kithara::hang_watchdog]` and its progress checks |
| Wasm wrappers | Add target-specific `wasm-bindgen-test = "=0.3.70"` and enable test-utils `subscriber` on `wasm32` |

With `hang` disabled, hang helpers are no-ops. An explicit test `timeout(...)`
is a separate timeout mechanism; it does not prove an active hang watchdog.
`hang_timeout_secs(N)` sets the budget for watchdog helpers called inside the
test. With `usdt` disabled, native consumers need no DTrace or USDT tooling.

The [standalone tests](../../xtask/tests/fixtures/test_utils.rs) build these
modes (minimal, mock, multithread, hang, config/derive) in a temporary workspace
outside the checkout; wasm wrapper checks establish compilation only. Run
`just test test-utils` for the matrix. To verify a published git revision, set
`KITHARA_TEST_UTILS_REV` to its full SHA and `KITHARA_TEST_UTILS_GIT_URL` to its
repository URL without credentials.

### Integration tests live elsewhere

The integration-test domain (synthetic HLS servers, `TestHttpServer`, `TestServerHelper`, `HlsFixtureBuilder`, the `hls_server` presets, …) lives in `kithara-integration-tests` (`tests/`), over the waveforms and encoded assets `kithara-test-fixtures` produces. To use it from another crate's tests, depend on `kithara-integration-tests` (it is `publish = false`) rather than re-implementing fixtures here.

See `tests/README.md` for the integration-test suite layout, the standalone `test_server` binary, the WASM flow, and the available fixture builders.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-test-utils) for detailed contracts, invariants, and internals.
