#![forbid(unsafe_code)]

mod coverage;
mod lru;
mod pin;

pub use coverage::{CoverageIndex, DiskCoverage};
pub use lru::EvictConfig;
pub(crate) use lru::LruIndex;
pub use pin::PinsIndex;
