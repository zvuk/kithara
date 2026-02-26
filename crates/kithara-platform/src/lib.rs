//! Platform-aware primitives for native and wasm32 targets.
//!
//! # Synchronization
//!
//! Re-exports [`wasm_safe_thread`] primitives (`Mutex`, `Condvar`, `RwLock`)
//! that adapt their locking strategy to the platform.
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
//! [`time::sleep`] delegates to `tokio::time::sleep` on native and to
//! `setTimeout` on wasm32.

mod blocking;
mod maybe_send;
mod pool;
mod task;
pub mod thread;
mod thread_pool_init;
pub mod time;

#[cfg(feature = "internal")]
pub mod internal;

pub use blocking::{BlockingError, BlockingHandle, spawn_blocking};
pub use maybe_send::{MaybeSend, MaybeSync, WasmSend};
pub use pool::ThreadPool;
pub use task::spawn_task;
pub use thread::{Duration, JoinHandle, backoff, spawn, yield_now};
pub use thread_pool_init::ensure_thread_pool;
pub use wasm_safe_thread::{
    Mutex,
    condvar::Condvar,
    guard::{Guard as MutexGuard, ReadGuard as RwLockReadGuard, WriteGuard as RwLockWriteGuard},
    mpsc,
    rwlock::RwLock,
    yield_to_event_loop_async,
};

#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
