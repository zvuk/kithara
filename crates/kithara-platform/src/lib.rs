//! Platform-aware primitives for native and wasm32 targets.
//!
//! Re-exports cross-platform [`sync`], [`thread`], [`time`], and [`tokio`]
//! primitives plus the [`MaybeSend`] / [`MaybeSync`] conditional trait
//! bounds. See the crate `README.md` for per-target backends.

mod cancel_group;
#[cfg(not(target_arch = "wasm32"))]
mod env;
mod logging;
mod maybe_send;
pub mod sync;
pub mod thread;
pub mod time;
pub mod tokio;

pub use cancel_group::CancelGroup;
#[cfg(not(target_arch = "wasm32"))]
pub use env::env_mutation_lock;
pub use logging::log_error;
pub use maybe_send::{BoxFuture, MaybeSend, MaybeSendFuture, MaybeSync, WasmSend};
pub use sync::{
    Condvar, Mutex, MutexGuard, NotAvailable, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
pub use thread::{
    Duration, JoinHandle, Thread, ThreadId, current, park, park_timeout, sleep, spawn,
};
