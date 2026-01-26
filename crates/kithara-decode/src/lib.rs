//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`Pipeline`] - Async pipeline running decoder in spawn_blocking
//! - [`SegmentStreamDecoder`] - Zero-copy decoder using SegmentSource trait
//! - [`SymphoniaDecoder`] - Symphonia-based audio decoder
//! - [`SourceReader`] - Sync Read+Seek adapter over async Source
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
//!
//! ## Segment-based Decoding (recommended for HLS)
//!
//! ```ignore
//! use kithara_decode::{SegmentStreamDecoder, SegmentSource};
//! use kithara_hls::HlsSegmentSource;
//!
//! // Create segment source
//! let source = HlsSegmentSource::new(/* ... */);
//!
//! // Create decoder
//! let mut decoder = SegmentStreamDecoder::new(Arc::new(source));
//!
//! // Decode loop
//! while let Some(chunk) = decoder.decode_next()? {
//!     play_audio(chunk);
//! }
//! ```
//!
//! ## Random-access Decoding (for progressive files)
//!
//! ```ignore
//! // Create pipeline from async Source
//! let mut pipeline = Pipeline::open(source_arc).await?;
//!
//! // Get buffer for playback
//! let buffer = pipeline.buffer();
//! ```

#![forbid(unsafe_code)]

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod chunked_reader;
mod decoder;
mod pcm_source;
mod pipeline;
pub mod resampler;
mod segment_decoder;
mod segment_source;
mod source_reader;
mod symphonia_mod;
mod traits;
mod types;

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use decoder::Decoder;
pub use pcm_source::PcmSource;
pub use pipeline::{PcmBuffer, Pipeline, PipelineCommand};
pub use segment_decoder::SegmentStreamDecoder;
pub use segment_source::{AudioCodec, ContainerFormat, SegmentId, SegmentInfo, SegmentSource};
pub use source_reader::SourceReader;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};
