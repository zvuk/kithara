//! # Kithara Decode
//!
//! Audio decoding library with generic sample type support.
//!
//! ## Architecture
//!
//! This crate is organized into several modules with clear separation of concerns:
//!
//! - [`types`] - Core types, errors, and traits
//! - [`symphonia_glue`] - Centralized Symphonia integration logic
//! - [`engine`] - Low-level Symphonia-based decode engine
//! - [`decoder`] - Decoder state machine wrapper
//! - [`pipeline`] - High-level async audio stream with backpressure
//! - [`test_helpers`] - Test utilities and fixtures (cfg(test) only)
//!
//! ## Sample Type `T`
//!
//! The generic parameter `T` represents the audio sample type. Common choices:
//! - `f32` - 32-bit float samples (recommended for processing)
//! - `i16` - 16-bit integer samples (common in audio files)
//!
//! `T` must implement these traits:
//! - `dasp::sample::Sample` - for sample manipulation
//! - `symphonia::core::audio::sample::Sample` - for Symphonia integration  
//! - `symphonia::core::audio::conv::ConvertibleSample` - for format conversion
//! - `Send + 'static` - for threading support
//!
//! ## PcmChunk Invariants
//!
//! [`PcmChunk<T>`] maintains these invariants:
//! - **Frame alignment**: `pcm.len() % channels == 0`
//! - **Valid specs**: `channels > 0` and `sample_rate > 0`
//! - **Interleaved layout**: Samples are stored as LRLRLR... for stereo
//!
//! Breaking these invariants will result in decode errors.
//!
//! ## Command Semantics
//!
//! [`DecodeCommand`] represents control operations:
//! - `Seek(Duration)` - Best-effort absolute seek
//!   - May not be frame-accurate depending on format
//!   - Next chunk reflects new position
//!   - Should not deadlock or hang
//!
//! ## Basic Usage
//!
//! ```rust,no_run,ignore
//! use kithara_decode::{Decoder, DecoderSettings};
//! use std::time::Duration;
//!
//! // Create decoder from media source
//! let settings = DecoderSettings::default();
//! let mut decoder = Decoder::<f32>::new(source, settings)?;
//!
//! // Decode audio
//! while let Some(chunk) = decoder.next()? {
//!     println!("Got {} frames of audio", chunk.frames());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Pipeline Usage
//!
//! For async processing with backpressure:
//! ```rust,no_run,ignore
//! use kithara_decode::{AudioStream, DecoderSettings};
//!
//! let mut stream = AudioStream::new(source, 10)?; // 10 chunk buffer
//!
//! while let Some(chunk) = stream.next_chunk().await? {
//!     // Process chunk...
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Pipeline Usage
//!
//! For async processing with backpressure:
//! ```rust,no_run,ignore
//! use kithara_decode::{AudioStream, DecoderSettings};
//!
//! let source = /* your AudioSource implementation */;
//! let mut stream = AudioStream::new(source, 10)?; // 10 chunk buffer
//!
//! while let Some(chunk) = stream.next_chunk().await? {
//!     // Process chunk...
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

pub use dasp::sample::Sample as DaspSample;
pub use symphonia::core::audio::conv::ConvertibleSample;
pub use symphonia::core::audio::sample::Sample as SymphoniaSample;

// Public API exports
pub use types::{
    AudioSource, AudioSpec, ChannelCount, DecodeCommand, DecodeError, DecodeResult,
    DecoderSettings, MediaSource, PcmChunk, PcmSpec, ReadSeek, SampleRate,
};

pub use decoder::Decoder;
pub use engine::DecodeEngine;
pub use pipeline::AudioStream;

// Internal modules
mod decoder;
mod engine;
mod pipeline;
mod symphonia_glue;
mod types;

// Test-only module
#[cfg(test)]
mod test_helpers;

// Re-export test helpers for convenience in tests
#[cfg(test)]
pub use test_helpers::{FakeAudioSource, TestMediaSource};
