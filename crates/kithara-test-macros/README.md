<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-test-macros.svg)](https://crates.io/crates/kithara-test-macros)
[![docs.rs](https://docs.rs/kithara-test-macros/badge.svg)](https://docs.rs/kithara-test-macros)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-test-macros

Proc-macro crate providing unified test attributes (`#[kithara::test]`, `#[kithara::probe]`, `#[kithara::mock]`) for native and wasm test suites. The attributes expand to no-ops in release builds, so production code can carry them as a normal dependency.

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

A bare `#[kithara::test]` is a sync test on native + wasm; flags can be combined (e.g. `#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]`).

- `tokio` — async test on native runtime
- `wasm` — wasm-only test
- `native` — native-only test
- `browser` — browser wasm test path; injects `kithara_platform::tokio::ensure_thread_pool().await` on WASM to initialize Web Workers before the body
- `timeout(Duration::...)` — wraps test body with a timeout
- `env(KEY = "value", ...)` — sets env vars before the body runs
- `tracing("directives")` — initializes tracing with a custom `EnvFilter` directive string (defaults to `warn` when omitted)
- `soft_fail("pattern", ...)` — catches panics whose message contains any given substring (case-insensitive), printing them as `[SOFT FAIL]` warnings; non-matching panics propagate. Requires `futures` at the call site for async tests
- `serial` — emits `#[serial_test::serial]` so the test never runs in parallel with other `serial` tests; for resource-intensive / contention-sensitive tests. Requires `serial_test` at the call site
- `multi_thread` — uses `new_multi_thread().worker_threads(2)` instead of `new_current_thread()`; required when the body spawns tasks needing a multi-threaded executor (e.g. `thirtyfour` `WebDriver`)
- `selenium` — convenience flag implying `native + tokio + serial + multi_thread` and adding `#[ignore = "requires selenium"]`, for Selenium/WebDriver integration tests

Supports `#[case]` / `#[case::name]` parameterization and fixture injection.

### Flash ambient holder per emit path

Every emitted test body is made flash-eligible by exactly ONE ambient holder:

- **async-native** emissions (manual tokio runtime, with or without `timeout`) wrap the body in `kithara_platform::flash::with_ambient`, which re-asserts `FLASH_AMBIENT` around every poll. They must NOT also hold a body scope: a body-held `ambient_scope` lives in the future's state inside the cancellable timeout and tears down non-LIFO on `Elapsed` — a stale ambient resurrect, caught by the platform's `restore_mode` guard.
- **native sync** and **wasm** emissions open the body with a single body-held `ambient_scope` (`shared::make_ambient_stmt`) — the sole ambient writer there.

## `#[kithara::probe(...)]` arguments

- `#[kithara::probe]` (no parens) — marker probe: emits only the cheap auto-fields (`seq`, `caller_file`, `caller_line`) and zero wire args. Use for very-frequent production functions whose parameters are not `IntoProbeArg` (e.g. `Future::poll_next(&self, cx: &mut Context)`).
- `#[kithara::probe(field1, field2, …)]` — explicit list of parameter idents to record as wire args (max 6, the USDT arity ceiling). Each ident must match a real parameter name.
- `#[kithara::probe(name = expr, …)]` — record a computed value under the wire-name `name`. `expr` is evaluated inside the function body at probe-firing time (so it can read `self`, parameters, locals); its result must implement `IntoProbeArg`. Plain idents and `name = expr` entries may be mixed; the combined count counts against the 6-arg ceiling.
- `#[kithara::probe(caller, …)]` — additionally capture `caller_fn` via `backtrace::trace`. Opt-in because backtrace resolution is ~ms per firing and blows up hot loops; do NOT use on `poll_next`-style hot probes.
- `#[kithara::probe(probe_return)]` — record the function's return value through `Probe::record_probe`.

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
