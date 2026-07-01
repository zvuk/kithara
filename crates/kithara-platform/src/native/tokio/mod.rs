//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how unvetted primitives
//! leak in unnoticed (see design doc §2). `time`/`net`/`fs`/`io`/`process`
//! and the root `spawn`/`spawn_blocking` are intentionally ABSENT.
//! `task_local!` is native-only (no task-scope surface in the wasm subset).
//! `signal` is opt-in for desktop binaries that own process shutdown.

#[cfg(feature = "signal")]
pub use backend::signal;
pub use backend::{join, pin, select, task_local, try_join};
pub(crate) use tokio as backend;

pub mod runtime;
pub mod sync;
pub mod task;
pub(crate) mod thread_pool;

pub use thread_pool::ensure_thread_pool;
