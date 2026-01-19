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
//! let audio_source = AudioSyncReader::new(audio_rx, resampler.output_spec());
//!
//! // Speed control (lock-free)
//! resampler.set_speed(1.5);
//! ```

#![forbid(unsafe_code)]

// Public API exports
#[cfg(feature = "rodio")]
pub use audio_sync_reader::AudioSyncReader;
pub use pipeline_v2::{AudioPipeline, PipelineCommand};
pub use resampler::{ResamplerCommand, ResamplerPipeline};
pub use source_reader::SourceReader;
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{DecodeError, DecodeResult, DecoderSettings, PcmChunk, PcmSpec};

// Internal modules
#[cfg(feature = "rodio")]
mod audio_sync_reader;
mod pipeline_v2;
pub mod resampler;
mod source_reader;
mod symphonia_mod;
mod types;
