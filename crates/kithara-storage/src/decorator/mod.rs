#![forbid(unsafe_code)]

//! Decorators that wrap a [`ResourceExt`](crate::ResourceExt) inner with
//! crash-safety policies (whole-file or chunked write-rename).

pub(crate) mod atomic;
pub(crate) mod chunked;

pub use atomic::Atomic;
#[cfg(not(target_arch = "wasm32"))]
pub use atomic::AtomicMmap;
pub use chunked::{AtomicChunked, OpenIntent};
