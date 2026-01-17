//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`AudioPipeline`] - Async pipeline running decoder in spawn_blocking
//! - [`SymphoniaDecoder`] - Symphonia-based audio decoder
//! - [`SourceReader`] - Sync Read+Seek adapter over async Source
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
//!
//! ## Usage
//!
//! ```ignore
//! // Create pipeline directly from async Source
//! let mut pipeline = AudioPipeline::open(source_arc).await?;
//!
//! // Get audio receiver for playback
//! let audio_rx = pipeline.take_audio_receiver().unwrap();
//! let audio_source = AudioSyncReader::new(audio_rx, pipeline.spec());
//! ```

#![forbid(unsafe_code)]

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use pipeline_v2::{AudioPipeline, PipelineCommand};
pub use source_reader::SourceReader;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod pipeline_v2;
mod source_reader;
mod symphonia_mod;
mod types;
