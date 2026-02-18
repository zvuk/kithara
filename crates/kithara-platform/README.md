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

<table>
<tr><th>Type</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>Mutex&lt;T&gt;</code></td><td><code>parking_lot::Mutex</code></td><td>Spin-loop <code>try_lock</code> wrapper</td></tr>
<tr><td><code>RwLock&lt;T&gt;</code></td><td><code>parking_lot::RwLock</code></td><td>Spin-loop reader-writer lock</td></tr>
<tr><td><code>Condvar</code></td><td><code>parking_lot::Condvar</code></td><td>Modified semantics for single-threaded wasm</td></tr>
<tr><td><code>MaybeSend</code></td><td>= <code>Send</code></td><td>No-op (auto-implemented)</td></tr>
<tr><td><code>MaybeSync</code></td><td>= <code>Sync</code></td><td>No-op (auto-implemented)</td></tr>
</table>

## Time Utilities

<table>
<tr><th>Function</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>sleep(duration)</code></td><td><code>tokio::time::sleep</code></td><td>No-op</td></tr>
</table>

## Integration

Foundation crate used across the entire kithara workspace. Allows higher-level crates (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-bufpool`, etc.) to share synchronization code between native and wasm32 builds without platform-specific `#[cfg]` blocks.
