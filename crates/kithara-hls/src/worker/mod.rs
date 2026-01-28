//! HLS worker architecture.
//!
//! This module provides a worker-based approach to HLS streaming where each message
//! contains complete metadata about the segment, codec, and boundaries.
//!
//! ## Architecture
//!
//! - `HlsMessage`: Message with full metadata (codec, segment info, encryption)
//! - `HlsCommand`: Control commands (seek, variant switch, pause/resume)
//! - `HlsWorkerSource`: Implementation of AsyncWorkerSource trait
//! - Helper types: VariantMetadata

mod adapter;
mod chunk;
mod command;
mod metadata;
mod source;

pub use adapter::{HlsBackend, HlsSourceAdapter};
pub use chunk::HlsMessage;
pub use command::HlsCommand;
pub use metadata::VariantMetadata;
pub use source::HlsWorkerSource;
