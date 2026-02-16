//! Platform-aware synchronization primitives.
//!
//! On native targets, re-exports [`parking_lot`] types directly.
//!
//! On `wasm32`, provides wrapper types that use `try_lock()` + spin loop
//! instead of blocking `lock()`. This avoids `Atomics.wait()` panics on
//! the browser main thread where `Atomics.wait` is forbidden.
//!
//! # Background
//!
//! `parking_lot` compiled with the `nightly` feature on `wasm32-unknown-unknown`
//! uses `Atomics.wait()` for contended locks. Web Workers can call
//! `Atomics.wait`, but the **browser main thread cannot** — it throws
//! `RuntimeError: Atomics.wait cannot be called in this context`.
//!
//! In Kithara's WASM build the HLS downloader runs on the main thread
//! (async I/O needs the browser event loop) while the decoder runs on a
//! rayon Web Worker. Any `parking_lot::Mutex::lock()` on the main thread
//! that contends with the worker will panic.
//!
//! The wasm32 wrappers use `try_lock()` in a spin loop. The rayon worker
//! holds locks for microseconds, so spins resolve quickly.

// On native: re-export parking_lot types directly (zero overhead).
#[cfg(not(target_arch = "wasm32"))]
mod native {
    pub use parking_lot::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

// On wasm32: wrapper types using try_lock + spin loop.
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::*;
