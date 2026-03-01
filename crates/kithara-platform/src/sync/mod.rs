//! Platform-optimal synchronization primitives.
//!
//! * **Native** — backed by [`parking_lot`] (no poisoning, smaller footprint).
//! * **WASM** — backed by [`wasm_safe_thread`] (safe on the main thread).

mod condvar;
pub mod mpsc;
mod mutex;
mod rwlock;

pub use condvar::{Condvar, WaitTimeoutResult};
pub use mutex::{Mutex, MutexGuard, NotAvailable};
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
