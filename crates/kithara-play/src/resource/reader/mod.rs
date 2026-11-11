mod core;

#[cfg(feature = "mock")]
pub(crate) mod mock;

pub(crate) use core::LaneGeometry;
pub use core::{OpenedTrack, Resource, ResourceLoad};
