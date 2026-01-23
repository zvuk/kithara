//! Stream-based HLS worker architecture.
//!
//! This module provides a worker-based approach to HLS streaming where each message
//! contains complete metadata about the segment, codec, and boundaries.
//!
//! ## Architecture
//!
//! - `HlsMessage`: Stream message with full metadata (codec, segment info, encryption)
//! - `HlsCommand`: Control commands (seek, variant switch, pause/resume)
//! - `HlsWorkerSource`: Implementation of AsyncWorkerSource trait
//! - Helper types: VariantMetadata, ThroughputAccumulator, BufferTracker

mod adapter;
mod buffer;
mod chunk;
mod command;
mod metadata;
mod segment_metadata;
mod source;
mod throughput;

pub use adapter::HlsSourceAdapter;
pub use buffer::BufferTracker;
pub use chunk::HlsMessage;
pub use command::HlsCommand;
pub use metadata::VariantMetadata;
pub use segment_metadata::HlsSegmentMetadata;
pub use source::HlsWorkerSource;
pub use throughput::{DownloadStats, ThroughputAccumulator};
