//! Mirror of the tokio surface the workspace is allowed to touch.
//! Enumerated on purpose: a glob re-export is how unvetted primitives
//! leak in unnoticed. `time`/`fs`/`process` and the root
//! `spawn`/`spawn_blocking` are intentionally ABSENT. `net` and minimal async IO
//! extension traits are feature-gated for native test/server socket plumbing.
//! `task_local!` is native-only (no task-scope surface in the wasm subset).
//! `signal` is opt-in for desktop binaries that own process shutdown.

#[cfg(feature = "signal")]
pub use backend::signal;
pub use backend::{join, main, pin, select, task_local, try_join};
pub(crate) use tokio as backend;

#[cfg(feature = "tokio-net")]
pub mod io {
    pub use super::backend::io::{AsyncReadExt, AsyncWriteExt};
}

#[cfg(feature = "tokio-net")]
pub mod net {
    pub use super::backend::net::{TcpListener, TcpStream};
}
pub mod runtime;
pub mod sync;
pub mod task;
pub(crate) mod thread_pool;

pub use thread_pool::ensure_thread_pool;
