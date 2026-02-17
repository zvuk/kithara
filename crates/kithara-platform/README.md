<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-platform.svg)](https://crates.io/crates/kithara-platform)
[![Downloads](https://img.shields.io/crates/d/kithara-platform.svg)](https://crates.io/crates/kithara-platform)
[![docs.rs](https://docs.rs/kithara-platform/badge.svg)](https://docs.rs/kithara-platform)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-platform

Platform-aware primitives for native and wasm32 targets. Provides synchronization types (`Mutex`, `RwLock`, `Condvar`), conditional trait bounds (`MaybeSend`, `MaybeSync`), and async utilities (`sleep`) that work transparently on both native and WebAssembly.

## Usage

```rust
use kithara_platform::{Mutex, Condvar, MaybeSend};

// Works on both native (parking_lot) and wasm32 (spin-loop fallback)
let lock = Mutex::new(42);
let guard = lock.lock();
```

## Key Types

| Type | Native | wasm32 |
|------|--------|--------|
| `Mutex<T>` | `parking_lot::Mutex` | Spin-loop `try_lock` wrapper |
| `RwLock<T>` | `parking_lot::RwLock` | Spin-loop reader-writer lock |
| `Condvar` | `parking_lot::Condvar` | Modified semantics for single-threaded wasm |
| `MaybeSend` | = `Send` | No-op (auto-implemented) |
| `MaybeSync` | = `Sync` | No-op (auto-implemented) |

## Time Utilities

| Function | Native | wasm32 |
|----------|--------|--------|
| `sleep(duration)` | `tokio::time::sleep` | No-op |

## Integration

Foundation crate used across the entire kithara workspace. Allows higher-level crates (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-bufpool`, etc.) to share synchronization code between native and wasm32 builds without platform-specific `#[cfg]` blocks.
