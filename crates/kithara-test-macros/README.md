<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-macros

Workspace proc-macro crate (`publish = false`) with unified test attributes for native + wasm test suites.

## Macros

- `#[kithara::test(...)]`
- `#[kithara::fixture]`

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
    "http://127.0.0.1:3333/hls/master.m3u8".to_string()
}
```

## Integration

Used by workspace tests (`tests/`, crate-local integration tests) to keep one test annotation model across native + wasm targets.
