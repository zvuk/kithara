#[path = "condvar.rs"]
pub(crate) mod condvar;
#[path = "mutex.rs"]
pub(crate) mod mutex;
#[path = "rwlock.rs"]
pub(crate) mod rwlock;

pub use std::sync::atomic;

pub(crate) use condvar::Condvar;
pub(crate) use mutex::{Mutex, MutexGuard};

pub use crate::system::ownership::{Arc, OnceLock, Weak};
