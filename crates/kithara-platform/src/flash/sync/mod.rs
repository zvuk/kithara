//! Flash-side sync primitives: re-import stubs where flash adds no semantics
//! (`Mutex`/`RwLock` — bounded critical sections stay flash-blind by
//! contract), flash-aware primitives where it does (`Condvar`, `Notify`,
//! `mpsc`).

pub(crate) mod condvar;
pub mod mpsc;
pub(crate) mod mutex;
pub(crate) mod notify;
pub(crate) mod rwlock;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexGuard, NotAvailable};
pub use notify::Notify;
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
