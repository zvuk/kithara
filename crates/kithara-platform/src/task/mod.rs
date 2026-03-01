//! Async task primitives (mirrors [`tokio::task`] layout).
//!
//! * [`spawn`] — fire-and-forget async task.
//! * [`spawn_blocking`] — run a closure on a blocking thread.
//! * [`yield_now`] — yield to the executor / event loop.

mod blocking;
mod spawn;
mod yield_now;

pub use blocking::{BlockingError, BlockingHandle, spawn_blocking};
pub use spawn::spawn;
pub use yield_now::yield_now;
