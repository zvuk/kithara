#![forbid(unsafe_code)]

//! Mmap-backed storage driver.
//!
//! The struct + factory live in `driver`, the I/O ops live in `io`.
//!
//! ## Performance optimizations
//!
//! - **Lock-free fast path**: Writer publishes ready ranges to `SegQueue`.
//!   Reader checks queue before blocking on mutex, reducing contention.
//! - **Fallback slow path**: If queue is empty, falls back to `Mutex + Condvar`
//!   for full synchronization.

pub(crate) mod driver;
pub(crate) mod io;

pub use driver::{MmapDriver, MmapOptions, MmapResource};
