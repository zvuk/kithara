//! HLS layered pipeline: base iteration with ABR and DRM, prefetch stage.
//!
//! Each stage implements the same `PipelineStream` trait to allow stacking.

pub mod base;
pub mod prefetch;
pub mod types;

pub use base::BaseStream;
pub use prefetch::PrefetchStream;
pub use types::{
    PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentMeta, SegmentPayload,
};
