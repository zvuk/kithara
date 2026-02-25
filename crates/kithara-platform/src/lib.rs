//! Platform-aware primitives for native and wasm32 targets.
//!
//! # Synchronization
//!
//! On native targets, re-exports [`parking_lot`] types directly.
//!
//! On `wasm32`, provides wrapper types that use `try_lock()` + spin loop
//! instead of blocking `lock()`. This avoids `Atomics.wait()` panics on
//! the browser main thread where `Atomics.wait` is forbidden.
//!
//! # Conditional trait bounds
//!
//! [`MaybeSend`] and [`MaybeSync`] are conditional trait bounds:
//! on native they equal `Send`/`Sync`, on wasm32 they are blanket-implemented
//! for all types. Use in trait bounds to avoid duplicating trait definitions
//! with `#[cfg]` gates.
//!
//! # Async utilities
//!
//! [`sleep`] delegates to `tokio::time::sleep` on native and is a no-op on
//! wasm32 (browser fetch has its own retry/timeout mechanisms).

mod blocking;
mod channel;
mod maybe_send;
mod pool;
mod task;
pub mod thread;
mod thread_pool_init;
pub mod time;

#[cfg(feature = "internal")]
pub mod internal;

pub use blocking::{BlockingError, BlockingHandle, spawn_blocking};
pub use channel::{ReceiveError, Receiver, SendError, Sender, bounded, unbounded};
pub use maybe_send::{MaybeSend, MaybeSync};
pub use pool::ThreadPool;
pub use task::spawn_task;
pub use thread::{Duration, JoinHandle, sleep, spawn, yield_now};
pub use thread_pool_init::ensure_thread_pool;

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
