//! Mirror of the tokio surface the workspace is allowed to touch, flash side.
//! Enumerated on purpose: a glob re-export is how flash-blind primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT. `runtime`,
//! `ensure_thread_pool`, `signal` and `task_local!` add no flash semantics and
//! are re-imported from `native`; the channels and `task` are flash-aware.

pub use crate::native::tokio::{join, net, pin, select, signal, task_local, try_join};

pub mod broadcast;
pub mod mpsc;
pub mod oncecell;
pub mod oneshot;
pub mod semaphore;
pub mod sync;
pub mod task;
mod unbounded;
pub mod watch;

pub use crate::native::tokio::{runtime, thread_pool::ensure_thread_pool};
