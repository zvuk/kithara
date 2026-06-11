//! Platform-aware primitives for native and wasm32 targets.
//!
//! Re-exports cross-platform [`sync`], [`thread`], [`time`], and [`tokio`]
//! primitives plus the [`MaybeSend`] / [`MaybeSync`] conditional trait
//! bounds. See the crate `README.md` for per-target backends.

mod cancel_group;
pub mod env;
pub mod flash;
mod logging;
mod maybe_send;
// Flash-ON wraps (not re-exports) some native items: `unreachable_pub` is
// structurally false-positive there; `dead_code` covers W1-interim unconsumed
// arms and dies with the W2 wrappers; `unused_imports` covers pub-use items in
// native backends that are shadowed by the flash facade arm. OFF lane keeps
// full coverage. See AGENTS.md "Non-Negotiables" legalized exceptions.
#[cfg_attr(
    all(not(target_arch = "wasm32"), feature = "flash"),
    expect(unreachable_pub, dead_code, unused_imports)
)]
mod native;
mod rt_cancel;
pub mod sync;
pub mod task;
pub mod thread;
pub mod time;
pub mod tokio;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use cancel_group::CancelGroup;
#[cfg(not(target_arch = "wasm32"))]
pub use env::mutation_lock as env_mutation_lock;
pub use logging::log_error;
pub use maybe_send::{BoxFuture, MaybeSend, MaybeSendFuture, MaybeSync, WasmSend};
pub use rt_cancel::CancellationToken;
pub use sync::{
    Condvar, Mutex, MutexGuard, NotAvailable, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
pub use thread::{
    Duration, JoinHandle, Thread, ThreadId, current, park, park_timeout, sleep, spawn,
};
