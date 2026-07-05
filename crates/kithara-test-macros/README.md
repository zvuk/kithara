<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-test-macros.svg)](https://crates.io/crates/kithara-test-macros)
[![docs.rs](https://docs.rs/kithara-test-macros/badge.svg)](https://docs.rs/kithara-test-macros)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-macros

Proc-macro crate providing unified test attributes (`#[kithara::test]`,
`#[kithara::probe]`, `#[kithara::mock]`) for native and wasm test suites. Probe
and mock emissions are gated behind `cfg(any(test, feature = "probe"))` and
`cfg(any(test, feature = "mock"))`; flash is gated by the `flash` feature.

## Macros

Attribute macros:

- `#[kithara::test(...)]` — unified async/wasm test attribute (see flags below)
- `#[kithara::fixture]` — rstest-style fixture helper
- `#[kithara::probe(...)]` — USDT probe-point emitter consumed by `kithara-test-utils::probes`
- `#[kithara::mock(...)]` — wraps trait or impl with `unimock` mock generation
- `#[kithara::hang_watchdog(...)]` — wraps test bodies with the hang-detector watchdog
- `#[kithara::flash]` / `#[kithara::flash(true|false)]` — dynamic-flash guard for production functions
- `#[kithara::facade_flash]` — facade-path variant of `flash`, re-exported by the `kithara` crate
- `#[kithara::rtsan_forbid_blocking]` — mark a function as an RTSan nonblocking entry point
- `#[kithara::rtsan_allow_blocking]` — permit a blocking function inside an RTSan nonblocking context

Derive macros:

- `#[derive(Probe)]` — generate probe glue for an enum
- `#[derive(IntoProbeArg)]` — generate the conversion required to pass a value as a probe argument

## `#[kithara::test]` flags

A bare `#[kithara::test]` is a sync test on native + wasm; flags can be combined
(e.g. `#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]`). Flags
include `tokio`, `wasm`, `native`, `browser`, `timeout(...)`, `env(...)`,
`tracing(...)`, `soft_fail(...)`, `serial`, `multi_thread`, `selenium`, and
`flash(true|false)`. Supports `#[case]` / `#[case::name]` parameterization and
fixture injection. See [CONTEXT.md](CONTEXT.md) for per-flag semantics and the
flash ambient-holder rules.

## `#[kithara::probe(...)]` arguments

A bare `#[kithara::probe]` is a marker probe (cheap auto-fields only); parenthesized forms record parameter idents, computed `name = expr` values, an opt-in `caller`, or `probe_return`, up to the 6-arg USDT ceiling. See [CONTEXT.md](CONTEXT.md) for the full argument contract.

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

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
