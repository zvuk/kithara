#![forbid(unsafe_code)]

//! In-memory storage driver backed by a growable byte pool buffer.
//!
//! The struct + factory live in `driver`, the I/O ops live in `io`,
//! the unit-tests live in `tests`.

pub(crate) mod driver;
pub(crate) mod io;
#[cfg(test)]
mod tests;

pub use driver::{MemDriver, MemOptions, MemResource};
