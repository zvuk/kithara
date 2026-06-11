//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how flash-blind primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT.

pub use tokio_with_wasm::alias::{join, net, pin, select, try_join};

pub mod runtime;
pub mod sync;
pub mod task;
pub(crate) mod thread_pool;

pub use thread_pool::ensure_thread_pool;
