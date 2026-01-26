//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`StreamDecoder`] - Streaming decoder using MediaSource/MediaStream
//! - [`Pipeline`] - Async pipeline for progressive files
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
//!
//! ## Pipeline (for random-access playback)
//!
//! ```ignore
//! let mut pipeline = Pipeline::open(source_arc).await?;
//! let buffer = pipeline.buffer();
//! ```

#![forbid(unsafe_code)]

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod decoder;
mod media_source;
mod pcm_source;
mod pipeline;
pub mod resampler;
mod source_reader;
mod stream_decoder;
mod symphonia_mod;
mod traits;
mod types;

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use decoder::Decoder;
pub use media_source::{AudioCodec, ContainerFormat, MediaInfo, MediaSource, MediaStream};
pub use pcm_source::PcmSource;
pub use pipeline::{PcmBuffer, Pipeline, PipelineCommand};
pub use source_reader::SourceReader;
pub use stream_decoder::StreamDecoder;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};
