<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/kithara-hang-detector.svg)](https://crates.io/crates/kithara-hang-detector)
[![docs.rs](https://docs.rs/kithara-hang-detector/badge.svg)](https://docs.rs/kithara-hang-detector)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-hang-detector

Hang watchdog primitives for long-running loops and worker tasks.

## Usage

```rust
use std::time::Duration;
use kithara_hang_detector::{HangDetector, hang_watchdog};

let mut detector = HangDetector::new("decode.loop", Duration::from_secs(5));

loop {
    detector.tick();
    // ... unit of work
    detector.reset();
}
```

Macro form:

```rust
use kithara_hang_detector::hang_watchdog;

hang_watchdog! {
    timeout: std::time::Duration::from_secs(5);
    thread: "decoder.worker";
    loop {
        // ... unit of work
        hang_tick!();
        hang_reset!();
    }
}
```

## Behavior by target

<table>
<tr><th>Target</th><th>Timeout behavior</th></tr>
<tr><td>native</td><td><code>tick()</code> panics on timeout (fail-fast for deadlocks/hangs)</td></tr>
<tr><td>wasm32</td><td>logs one error per timeout window and resets deadline (avoids killing worker with abort panic)</td></tr>
</table>

Default timeout is 10s. On native, it can be overridden via `KITHARA_HANG_TIMEOUT_SECS`.

## API

- `HangDetector::new(label, timeout)`
- `HangDetector::tick()`
- `HangDetector::reset()`
- `default_timeout()`
- `hang_watchdog!` macro (`thread:` and `timeout:` options)

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>disable-hang-detector</code></td><td>no</td><td>No-op implementation (zero-cost stub for production tuning/tests)</td></tr>
</table>

## Integration

Re-exported by `kithara-platform` and used by runtime loops in playback/streaming crates to detect stalls early during development and testing.
