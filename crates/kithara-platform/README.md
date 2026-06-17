<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-platform.svg)](https://crates.io/crates/kithara-platform)
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
    let mut guard = lock.lock();
    *guard += 1;
}

time::sleep(std::time::Duration::from_millis(10)).await;
```

## Key types and entry points

- `Mutex<T>` / `RwLock<T>` / `Condvar` — synchronization wrappers over `parking_lot` (native) or `wasm_safe_thread` (wasm32).
- `MaybeSend` / `MaybeSync` — conditional trait bounds (`Send`/`Sync` on native, blanket no-op on wasm32).
- `thread::{spawn, spawn_named, is_main_thread, is_worker_thread, assert_main_thread, park_timeout, paced_backoff, unpark}` — thread primitives with thread-affinity helpers.
- `tokio::task::{spawn, spawn_blocking, yield_now}` — runtime task primitives (native `tokio`, worker-aware on wasm).
- `time::{sleep, timeout, Instant, real_io, reset}` — timing helpers; the single timestamp source (`web_time` / `std::time` are banned outside this crate).
- `CancelToken` — the single workspace cancellation token; `CancelScope`, `CancelGroup`, `CancelToken::{root, never, child}`.
- `flash` — off-by-default, native, test-only cargo feature that swaps the wall clock for a deterministic virtual timeline.

## Integration

Foundation crate used across the workspace (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-play`, `kithara-ffi`, and test infrastructure) to keep platform-specific branching isolated in one place.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
