#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod index;
mod mem;

pub use index::{CoverageIndex, DiskCoverage};
pub use mem::{Coverage, MemCoverage};
