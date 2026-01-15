//! HLS segment stream: variant selection, segment iteration, ABR, and commands.

mod pipeline;
pub mod types;

pub use pipeline::SegmentStream;
pub use types::{PipelineError, PipelineResult, SegmentMeta};
