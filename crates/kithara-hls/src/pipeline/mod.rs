//! HLS layered pipeline: base iteration, ABR overlay, DRM overlay, prefetch stage.
//!
//! Each stage implements the same `SegmentStream` trait to allow stacking.

pub mod abr;
pub mod base;
pub mod drm;
pub mod prefetch;
pub mod types;

pub use abr::AbrStream;
pub use base::BaseStream;
pub use drm::DrmStream;
pub use prefetch::PrefetchStream;
pub use types::{
    PipelineCommand, PipelineError, PipelineEvent, PipelineResult, SegmentMeta, SegmentPayload,
    SegmentStream,
};
