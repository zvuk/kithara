//! Mirror of the tokio surface the workspace is allowed to touch, flash side.
//! Enumerated on purpose: a glob re-export is how flash-blind primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT. `runtime` and
//! `ensure_thread_pool` add no flash semantics and are re-imported from
//! `native`; the channels and `task` are flash-aware.

pub use tokio_with_wasm::alias::{join, net, pin, select, try_join};

pub mod mpsc;
pub mod oneshot;
pub mod sync;
pub mod task;
mod unbounded;

pub use crate::native::tokio::{runtime, thread_pool::ensure_thread_pool};
