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
//! use ringbuf::traits::Consumer;
//!
//! // HLS stream with decoding
//! let config = AudioConfig::<Hls>::new(hls_config);
//! let mut audio = Audio::<Stream<Hls>>::new(config).await?;
//! sink.append(audio);  // rodio compatible
//!
//! // Or read PCM from channel directly
//! while let Some(chunk) = audio.pcm_rx().try_pop() {
//!     play_audio(chunk);
//! }
//!
//! // Events via event_bus()
//! let mut events = audio.event_bus().subscribe();
//! while let Ok(event) = events.recv().await {
//!     println!("Audio: {:?}", event);
//! }
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

// Internal modules
mod audio;
pub mod effects;
#[cfg(any(test, feature = "test-utils"))]
pub mod mock;
mod pipeline;
mod resampler;
#[cfg(feature = "rodio")]
mod rodio;
mod traits;
pub(crate) mod worker;

#[cfg(feature = "internal")]
pub mod internal;

// Public API exports
pub use audio::Audio;
pub use effects::eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands};
pub use pipeline::config::AudioConfig;
pub use resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality};
pub use traits::{AudioEffect, DecodeError, DecodeResult, PcmReader};
pub use worker::{handle::AudioWorkerHandle, types::ServiceClass};
