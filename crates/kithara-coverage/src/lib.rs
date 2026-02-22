#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod index;
mod manager;
mod mem;

#[cfg(feature = "internal")]
pub mod internal;

pub use index::{CoverageIndex, DiskCoverage};
pub use manager::CoverageManager;
pub use mem::{Coverage, MemCoverage};
