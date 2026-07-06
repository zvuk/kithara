#![forbid(unsafe_code)]

//! Decorators that wrap a [`ResourceWriter`](crate::ResourceWriter) inner with
//! crash-safety policies (whole-file or chunked write-rename).

pub(crate) mod atomic;
pub(crate) mod chunked;

pub use atomic::Atomic;
pub use chunked::{AtomicChunked, OpenIntent};
