#![forbid(unsafe_code)]

mod byte_map;
mod probe;
mod read;
mod traits;
mod variant;

pub use byte_map::ByteMap;
pub use probe::SourceProbe;
pub use read::{NotReadyCause, PendingReason, ReadOutcome, SourcePhase};
#[cfg(any(test, feature = "mock"))]
pub use traits::SourceMock;
pub use traits::{SegmentDescriptor, Source, SourceSeekAnchor};
pub use variant::VariantControl;
