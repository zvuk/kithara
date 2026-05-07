//! Platform-aware primitives for native and wasm32 targets.
//!
//! # Synchronization
//!
//! Re-exports [`sync`] primitives (`Mutex`, `Condvar`, `RwLock`, `mpsc`)
//! backed by [`parking_lot`] / `std` on native and [`wasm_safe_thread`] on WASM.
//!
//! # Async tasks
//!
//! [`tokio::task`] module: [`tokio::task::spawn`],
//! [`tokio::task::spawn_blocking`], [`tokio::task::yield_now`].
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

mod cancel_group;
#[cfg(not(target_arch = "wasm32"))]
mod env;
mod maybe_send;
pub mod sync;
pub mod thread;
pub mod time;
pub mod tokio;

pub use cancel_group::CancelGroup;
#[cfg(not(target_arch = "wasm32"))]
pub use env::env_mutation_lock;
pub use kithara_hang_detector::{HangDetector, default_timeout, hang_watchdog};
pub use maybe_send::{BoxFuture, MaybeSend, MaybeSendFuture, MaybeSync, WasmSend};
pub use sync::{
    Condvar, Mutex, MutexGuard, NotAvailable, RwLock, RwLockReadGuard, RwLockWriteGuard,
    WaitTimeoutResult,
};
pub use thread::{
    Duration, JoinHandle, Thread, ThreadId, current, park, park_timeout, sleep, spawn,
};
