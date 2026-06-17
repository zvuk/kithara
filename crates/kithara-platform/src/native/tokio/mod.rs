//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how unvetted primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT. `signal` and
//! `task_local!` are native-only (no socket/task-scope surface in the wasm
//! subset — same rationale as `net`).

pub use tokio_with_wasm::alias::{join, net, pin, select, signal, task_local, try_join};

pub mod runtime;
pub mod sync;
pub mod task;
pub(crate) mod thread_pool;

pub use thread_pool::ensure_thread_pool;
