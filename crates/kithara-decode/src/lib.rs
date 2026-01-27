//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`StreamDecoder`] - Streaming decoder using MediaSource/MediaStream
//! - [`SymphoniaDecoder`] - Symphonia-based audio decoder
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
//!
//! ## Streaming Decode (HLS, progressive files)
//!
//! ```ignore
//! use kithara_decode::{StreamDecoder, MediaSource};
//! use kithara_hls::HlsMediaSource;
//!
//! // Open media source
//! let source = HlsMediaSource::new(/* ... */);
//! let stream = source.open()?;
//!
//! // Create decoder
//! let mut decoder = StreamDecoder::new(stream)?;
//!
//! // Decode loop - bytes read on-demand, may block waiting for network
//! while let Some(chunk) = decoder.decode_next()? {
//!     play_audio(chunk);
//! }
//! ```

#![forbid(unsafe_code)]

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod decode_pipeline;
mod decoder;
mod media_source;
mod source_reader;
mod stream_decoder;
mod symphonia_mod;
mod types;

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use decode_pipeline::{DecodePipeline, DecodePipelineConfig};
pub use decoder::Decoder;
pub use media_source::{AudioCodec, ContainerFormat, MediaInfo, MediaSource, MediaStream};
pub use source_reader::SourceReader;
pub use stream_decoder::StreamDecoder;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};
