//! Platform-optimal synchronization primitives (wasm32), backed by
//! [`wasm_safe_thread`] (safe on the main thread).

pub(crate) mod condvar;
pub mod mpsc;
pub(crate) mod mutex;
pub(crate) mod notify;
pub(crate) mod rwlock;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexGuard, NotAvailable};
pub use notify::Notify;
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use crate::common::gate::{CondvarGate, ThreadGate, WaitGate};
