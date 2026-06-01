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

## CancellationToken

`CancellationToken` is the **single** cancellation token type across the workspace —
crates never depend on `tokio_util` directly; they use this one. It is a real-time-safe
token: a `tokio_util::CancellationToken` (async `cancelled()` side + the wider
propagation tree) paired with a **chain of per-node cancel flags**. `tokio_util`'s own
`is_cancelled()` takes the `TreeNode` `Mutex<Inner>` (no atomic fast path), which traps
under RealtimeSanitizer on the audio produce-core. `CancellationToken::is_cancelled()`
walks `self → root` loading each node's flag (`Acquire`) — lock-free, wait-free, bounded
by tree depth (master → consumer is 2–3 nodes), RT-safe.

- `cancel()` `Release`-stores **this node's** flag **then** calls `inner.cancel()`, so a
  thread observing the inner async cancellation (or any later `Acquire` walk) is
  guaranteed to see the flag set; the flag can never lag the inner token.
- `child_token()` creates a **new** flag node linked to the parent's chain. A
  parent/master `cancel()` is observed by every descendant's lock-free `is_cancelled()`
  (the descendant's walk reaches the parent's flag), while cancelling a child or sibling
  never marks the parent cancelled — the correct hierarchy. `Clone` keeps the **same**
  node (same identity), like `tokio_util`'s clone.
- `cancelled()` / `cancelled_owned()` delegate to the inner token (async side unchanged),
  so the token drops into `tokio::select!` exactly like a `tokio_util` token.

Consumer-crate masters (`Queue`, `App`, FFI, `PlayerImpl` fallback) mint a root
`CancellationToken` (via `new()` / `default()`) and thread `child_token()`s down the
player → audio-worker and player → HLS-coord chains so a master `cancel()` is seen by the
worker's lock-free read. `new()` / `default()` root a fresh hierarchy and belong only at
marked owner sites (see the `AGENTS.md` cancel-hierarchy contract, enforced by
`cargo xtask lint arch`).

## Trait Bridges

- `CancellationToken` → `CancelGroup` (`From`) — single-token group
- `Vec<CancellationToken>` → `CancelGroup` (`From`) — multi-token group
- `NotAvailable` / `TimeoutError` / `JoinError` / `TryCurrentError` (`Display`) — human-readable error rendering

## Integration

Foundation crate used across the workspace (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-play`, `kithara-wasm`, and test infrastructure) to keep platform-specific branching isolated in one place.
