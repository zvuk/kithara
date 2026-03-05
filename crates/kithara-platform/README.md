<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/kithara-platform.svg)](https://crates.io/crates/kithara-platform)
[![docs.rs](https://docs.rs/kithara-platform/badge.svg)](https://docs.rs/kithara-platform)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-platform

Platform-aware runtime primitives for native and `wasm32` targets. This crate is the portability boundary for synchronization, thread/task spawning, timing helpers, and conditional trait bounds used across the workspace.

## Usage

```rust
use kithara_platform::{Mutex, time};

let lock = Mutex::new(42_u32);
{
    let mut guard = lock.lock_sync();
    *guard += 1;
}

time::sleep(std::time::Duration::from_millis(10)).await;
```

## Synchronization Backends

<table>
<tr><th>Type</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>Mutex&lt;T&gt;</code></td><td><code>parking_lot::Mutex</code> wrapper</td><td><code>wasm_safe_thread::Mutex</code> wrapper</td></tr>
<tr><td><code>RwLock&lt;T&gt;</code></td><td><code>parking_lot::RwLock</code> wrapper</td><td><code>wasm_safe_thread::rwlock::RwLock</code> wrapper</td></tr>
<tr><td><code>Condvar</code></td><td><code>parking_lot::Condvar</code> wrapper</td><td><code>wasm_safe_thread::condvar::Condvar</code> wrapper</td></tr>
<tr><td><code>MaybeSend</code></td><td>Equivalent to <code>Send</code></td><td>Blanket trait (no Send requirement)</td></tr>
<tr><td><code>MaybeSync</code></td><td>Equivalent to <code>Sync</code></td><td>Blanket trait (no Sync requirement)</td></tr>
</table>

## Thread and Task Primitives

<table>
<tr><th>API</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>thread::spawn</code></td><td><code>std::thread::spawn</code></td><td><code>wasm_safe_thread::Builder::spawn</code> (Web Worker)</td></tr>
<tr><td><code>tokio::task::spawn</code></td><td><code>tokio_with_wasm</code> alias runtime</td><td>Worker-aware wrapper with lifecycle hooks</td></tr>
<tr><td><code>tokio::task::spawn_blocking</code></td><td><code>tokio_with_wasm</code> blocking pool</td><td>Dedicated worker-thread execution</td></tr>
<tr><td><code>thread::is_main_thread / is_worker_thread</code></td><td>always main / false</td><td>detects browser main vs Worker global scope</td></tr>
<tr><td><code>thread::assert_main_thread / assert_not_main_thread</code></td><td>no-op</td><td>panic on wrong thread affinity</td></tr>
</table>

## Time Utilities

- `time::sleep(duration)`
- `time::timeout(duration, future)`
- `time::Instant` (via `web-time`)

On native these delegate to `tokio` runtime primitives, on wasm they use `setTimeout`-based scheduling.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Enables</th></tr>
<tr><td><code>disable-hang-detector</code></td><td>no</td><td>No-op hang watchdog in dependent crates</td></tr>
<tr><td><code>internal</code></td><td>no</td><td>Internal-only exports for workspace debugging/testing</td></tr>
</table>

## Integration

Foundation crate used across the workspace (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-play`, `kithara-wasm`, and test infrastructure) to keep platform-specific branching isolated in one place.
