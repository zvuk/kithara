//! Platform-optimal synchronization primitives (native), backed by
//! [`parking_lot`] (no poisoning, smaller footprint).

pub(crate) mod condvar;
pub mod mpsc;
pub(crate) mod mutex;
pub(crate) mod notify;
pub(crate) mod rwlock;

pub use std::sync::atomic;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexGuard, NotAvailable};
pub use notify::Notify;
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use crate::{
    common::gate::{CondvarGate, ThreadGate, WaitGate},
    system::ownership::{Arc, OnceLock, Weak},
};
