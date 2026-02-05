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
// Legacy decoder (deprecated, use Symphonia<C> or DecoderFactory instead)
#[allow(deprecated)]
mod legacy;
mod symphonia;
mod traits;
mod types;

// Legacy exports (deprecated, kept for backward compatibility)
#[allow(deprecated)]
pub use legacy::{CachedCodecInfo, Decoder, InnerDecoder};
// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
pub use types::{DecodeError, DecodeResult, PcmChunk, PcmSpec, TrackMetadata};
