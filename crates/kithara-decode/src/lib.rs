//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`AudioPipeline`] - Async pipeline running decoder in spawn_blocking
//! - [`SymphoniaDecoder`] - Symphonia-based audio decoder
//! - [`SourceReader`] - Sync Read+Seek adapter over async Source
//! - [`ResamplerPipeline`] - Sample rate conversion and speed control
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
//!
//! ## Usage
//!
//! ```ignore
//! // Create pipeline directly from async Source
//! let mut pipeline = AudioPipeline::open(source_arc).await?;
//!
//! // Optional: add resampler for sample rate conversion / speed control
//! let decoder_rx = pipeline.take_audio_receiver().unwrap();
//! let mut resampler = ResamplerPipeline::new(decoder_rx, pipeline.spec(), 44100);
//! let audio_rx = resampler.take_audio_receiver().unwrap();
//!
//! // Create rodio adapter
//! let audio_source = AudioSyncReader::new(consumer, buffer, spec);
//!
//! // Speed control (lock-free)
//! resampler.set_speed(1.5);
//! ```

#![forbid(unsafe_code)]

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use decoder::Decoder;
pub use decoder_stream::GenericStreamDecoder;
pub use pcm_source::PcmSource;
pub use pipeline::{PcmBuffer, Pipeline, PipelineCommand};
pub use source_reader::SourceReader;
pub use stream_decoder::StreamDecoder;
pub use stream_pipeline::StreamPipeline;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod decoder;
mod decoder_stream;
mod pcm_source;
mod pipeline;
pub mod resampler;
mod source_reader;
mod stream_decoder;
mod stream_pipeline;
mod symphonia_mod;
mod traits;
mod types;
