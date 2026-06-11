//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how flash-blind primitives
//! leak in unnoticed (see design doc §2). `time`/`fs`/`io`/`process` and the
//! root `spawn`/`spawn_blocking` are intentionally ABSENT.
pub use tokio_with_wasm::alias::{join, pin, select, try_join};
pub mod runtime;
pub mod sync;
pub mod task;
pub use tokio_with_wasm::alias::net;

#[cfg(not(target_arch = "wasm32"))]
pub use crate::native::tokio::thread_pool::ensure_thread_pool;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::tokio::thread_pool::ensure_thread_pool;
