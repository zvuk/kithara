//! # Kithara Audio
//!
//! Audio pipeline library with decoding, effects, and resampling.
//!
//! ## Architecture
//!
//! - [`Audio`] - Generic audio pipeline running in a separate thread
//! - [`AudioGenerator`] - Trait for PCM audio sources
//! - [`AudioEffect`] - Trait for audio processing effects
//! - [`ResamplerProcessor`] - Sample rate conversion effect
//! - [`AudioSyncReader`] - rodio::Source adapter (requires `rodio` feature)
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

// Internal modules
mod events;
mod pipeline;
mod reader;
mod resampler;
#[cfg(feature = "rodio")]
mod rodio_impl;
#[cfg(feature = "rodio")]
mod sync;
mod traits;
mod types;

// Re-export kithara-decode types for convenience
// Public API exports
pub use events::{AudioEvent, AudioPipelineEvent};
pub use kithara_decode::{
    AudioCodec, CachedCodecInfo, ContainerFormat, Decoder, InnerDecoder, MediaInfo, PcmChunk,
    PcmSpec, TrackMetadata,
};
pub use pipeline::{Audio, AudioConfig};
pub use reader::SourceReader;
pub use resampler::ResamplerProcessor;
#[cfg(feature = "rodio")]
pub use sync::AudioSyncReader;
pub use traits::{AudioEffect, AudioGenerator};
pub use types::{DecodeError, DecodeResult, PcmReader};
