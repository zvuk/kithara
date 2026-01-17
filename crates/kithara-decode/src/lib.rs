//! # Kithara Decode
//!
//! Audio decoding library built on Symphonia.
//!
//! ## Architecture
//!
//! - [`types`] - Core types, errors, and traits
//! - [`symphonia`] - Symphonia-based decoding (SymphoniaDecoder, convert utilities)
//!
//! ## Sample Types
//!
//! Primary output is `f32` (recommended for processing).
//! `convert_to_i16()` available for i16 output when needed.
//!
//! ## PcmChunk Invariants
//!
//! [`PcmChunk<T>`] maintains these invariants:
//! - **Frame alignment**: `pcm.len() % channels == 0`
//! - **Valid specs**: `channels > 0` and `sample_rate > 0`
//! - **Interleaved layout**: Samples are stored as LRLRLR... for stereo

#![forbid(unsafe_code)]

// Public API exports
pub use symphonia_mod::{CachedCodecInfo, SymphoniaDecoder};
pub use types::{
    AudioSource, AudioSpec, ChannelCount, DecodeCommand, DecodeError, DecodeResult,
    DecoderSettings, MediaSource, PcmChunk, PcmSpec, ReadSeek, SampleRate,
};

// Internal modules
mod symphonia_mod;
mod types;

// Legacy modules (MVP placeholders, to be removed)
mod decoder;
mod engine;
mod pipeline;
mod symphonia_glue;

pub use decoder::Decoder;
pub use engine::DecodeEngine;
pub use pipeline::AudioStream;

// Test-only module
#[cfg(test)]
mod test_helpers;

// Re-export test helpers for convenience in tests
#[cfg(test)]
pub use test_helpers::{FakeAudioSource, TestMediaSource};
