//! Platform-aware primitives for native and wasm32 targets.
//!
//! # Synchronization
//!
//! Re-exports [`sync`] primitives (`Mutex`, `Condvar`, `RwLock`, `mpsc`)
//! backed by [`parking_lot`] / `std` on native and [`wasm_safe_thread`] on WASM.
//!
//! # Async tasks
//!
//! [`task`] module mirrors [`tokio::task`] layout: [`task::spawn`],
//! [`task::spawn_blocking`], [`task::yield_now`].
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

mod maybe_send;
pub mod sync;
pub mod task;
pub mod thread;
pub mod time;

#[cfg(feature = "internal")]
pub mod internal;

pub use kithara_hang_detector::{HangDetector, hang_watchdog};
pub use maybe_send::{MaybeSend, MaybeSync, WasmSend};
pub use sync::{
    Condvar, Mutex, MutexGuard, NotAvailable, RwLock, RwLockReadGuard, RwLockWriteGuard,
    WaitTimeoutResult,
};
// Backward-compatible re-exports for the rename.
pub use task::spawn as spawn_task;
pub use task::{BlockingError, BlockingHandle, spawn_blocking, yield_now};
pub use thread::{Duration, JoinHandle, sleep, spawn};

#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
