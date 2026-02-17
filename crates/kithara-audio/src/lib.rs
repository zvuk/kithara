//! # Kithara Audio
//!
//! Audio pipeline library with decoding, effects, and resampling.
//!
//! ## Architecture
//!
//! - [`Audio`] - Generic audio pipeline running in a separate thread
//! - [`AudioConfig`] - Pipeline configuration
//! - [`ResamplerQuality`] - Sample rate conversion quality
//! - `Audio` also implements `rodio::Source` directly (requires `rodio` feature)
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
//! // Events via decode_events()
//! let mut events = audio.decode_events();
//! while let Ok(event) = events.recv().await {
//!     println!("Audio: {:?}", event);
//! }
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

// Internal modules
pub mod effects;
#[cfg(any(test, feature = "test-utils"))]
pub mod mock;
mod pipeline;
mod resampler;
#[cfg(feature = "rodio")]
mod rodio;
mod traits;

// Public API exports
pub use effects::eq::{EqBandConfig, EqEffect, generate_log_spaced_bands};
pub use pipeline::{Audio, AudioConfig};
pub use resampler::ResamplerQuality;
#[doc(hidden)]
pub use resampler::{ResamplerParams, ResamplerProcessor};
#[doc(hidden)]
pub use traits::AudioEffect;
pub use traits::{DecodeError, DecodeResult, PcmReader};
