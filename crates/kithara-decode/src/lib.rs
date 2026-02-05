//! # Kithara Decode
//!
//! Pure audio decoding library built on Symphonia.
//!
//! Provides the low-level decoder that converts compressed audio formats
//! (AAC, MP3, FLAC, WAV, etc.) into PCM samples.
//!
//! For the full audio pipeline (threading, channels, seek, resampling),
//! see `kithara-audio`.

#![forbid(unsafe_code)]

mod error;
mod factory;
mod symphonia;
mod traits;
mod types;

// Error types
pub use error::{DecodeError, DecodeResult};

// Core types
pub use types::{PcmChunk, PcmSpec, TrackMetadata};

// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
