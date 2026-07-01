//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how unvetted primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT. `net` too: the
//! wasm tree is a documented subset — browser code has no socket surface.

pub use backend::{join, pin, select, try_join};
pub(crate) use tokio_with_wasm::alias as backend;

pub mod runtime;
pub mod sync;
pub mod task;
pub(crate) mod thread_pool;

pub use thread_pool::ensure_thread_pool;
