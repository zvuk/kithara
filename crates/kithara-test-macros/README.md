<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-macros

Workspace proc-macro crate (`publish = false`) with unified test attributes for native + wasm test suites.

## Macros

Attribute macros:

- `#[kithara::test(...)]` — unified async/wasm test attribute (see flags below)
- `#[kithara::fixture]` — rstest-style fixture helper
- `#[kithara::probe(...)]` — USDT probe-point emitter consumed by `kithara-test-utils::probes`
- `#[kithara::mock(...)]` — wraps trait or impl with `unimock` mock generation
- `#[kithara::hang_watchdog(...)]` — wraps test bodies with the hang-detector watchdog

Derive macros:

- `#[derive(Probe)]` — generate probe glue for an enum
- `#[derive(IntoProbeArg)]` — generate the conversion required to pass a value as a probe argument

## `#[kithara::test]` flags

- `tokio` — async test on native runtime
- `wasm` — wasm-only test
- `native` — native-only test
- `browser` — browser wasm test path (worker/thread-pool aware)
- `timeout(Duration::...)` — wraps test body with timeout
- `env(KEY = "value", ...)` — sets env vars for test body
- `soft_fail("pattern", ...)` — converts matching panics to warnings
- `serial` — emits `#[serial_test::serial]`

## Example

```rust
#[kithara::test(tokio, browser, timeout(std::time::Duration::from_secs(30)))]
async fn plays_hls_in_browser() {
    // test body
}

#[kithara::fixture]
fn temp_playlist() -> String {
    "http://127.0.0.1:3444/assets/hls/master.m3u8".to_string()
}
```

## Integration

Used by workspace tests (`tests/`, crate-local integration tests) to keep one test annotation model across native + wasm targets.
