//! Mirror of the tokio surface the workspace is allowed to touch, flash side.
//! Enumerated on purpose: a glob re-export is how flash-blind primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`process` and the root
//! `spawn`/`spawn_blocking` are intentionally ABSENT. `runtime`, feature-gated
//! `net`/minimal `io`, `ensure_thread_pool` and `task_local!` add no flash
//! semantics and are re-imported from `native`; the channels and `task` are
//! flash-aware.

#[cfg(feature = "signal")]
pub use crate::backend::tokio::signal;
#[cfg(feature = "tokio-net")]
pub use crate::backend::tokio::{io, net};
pub use crate::backend::tokio::{join, main, pin, select, task_local, try_join};

pub mod broadcast;
mod errors;
pub mod mpsc;
pub mod oncecell;
pub mod oneshot;
pub mod semaphore;
pub mod sync;
pub mod task;
mod unbounded;
pub mod watch;

pub use crate::backend::tokio::{runtime, thread_pool::ensure_thread_pool};
