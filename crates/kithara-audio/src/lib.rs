//! # Kithara Audio
//!
//! Audio pipeline library with decoding, effects, and resampling.
//!
//! ## Architecture
//!
//! - [`Audio`] - Generic audio pipeline running in a separate thread
//! - [`AudioConfig`] - Pipeline configuration
//! - [`ResamplerQuality`] - Sample rate conversion quality
//! - [`AudioSyncReader`] - `rodio::Source` adapter (requires `rodio` feature)
//!
//! ## Target API
//!
//! ```ignore
//! use kithara_audio::{Audio, AudioConfig};
//! use kithara_hls::{Hls, HlsConfig};
//! use kithara_stream::Stream;
//!
//! // HLS stream with decoding
//! let config = AudioConfig::<Hls>::new(hls_config);
//! let audio = Audio::<Stream<Hls>>::new(config).await?;
//! sink.append(audio);  // rodio compatible
//!
//! // Or read PCM from channel directly
//! while let Ok(chunk) = audio.pcm_rx().recv() {
//!     play_audio(chunk);
//! }
//!
//! // Events
//! let mut events = audio.events();
//! while let Ok(event) = events.recv().await {
//!     match event {
//!         AudioPipelineEvent::Stream(e) => println!("Stream: {:?}", e),
//!         AudioPipelineEvent::Audio(e) => println!("Audio: {:?}", e),
//!     }
//! }
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

// Internal modules
mod events;
mod pipeline;
mod reader;
mod resampler;
#[cfg(feature = "rodio")]
mod rodio;
mod traits;
mod types;

// Public API exports
pub use events::{AudioEvent, AudioPipelineEvent};
pub use kithara_decode::{
    AudioCodec, ContainerFormat, MediaInfo, PcmChunk, PcmSpec, TrackMetadata,
};
pub use pipeline::{Audio, AudioConfig};
pub use resampler::ResamplerQuality;
#[cfg(feature = "rodio")]
pub use rodio::AudioSyncReader;
pub use types::{DecodeError, DecodeResult, PcmReader};

// Hidden re-exports (used by integration tests or advanced internal consumers)
#[doc(hidden)]
pub use kithara_decode::{DecoderConfig, DecoderFactory, DecoderInput, InnerDecoder};
#[doc(hidden)]
pub use reader::SourceReader;
#[doc(hidden)]
pub use resampler::{ResamplerParams, ResamplerProcessor};
#[doc(hidden)]
pub use traits::{AudioEffect, AudioGenerator};
