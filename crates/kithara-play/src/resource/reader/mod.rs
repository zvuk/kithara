mod core;

#[cfg(feature = "mock")]
pub(crate) mod mock;

pub use core::{OpenedTrack, Resource, ResourceLoad};
