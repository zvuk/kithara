//! # Kithara Decode
//!
//! Audio decoding library with pluggable backends.
//!
//! Provides generic decoder infrastructure supporting Symphonia (software),
//! Apple AudioToolbox, and Android MediaCodec backends.
//!
//! ## Usage
//!
//! For static type selection:
//! ```ignore
//! use kithara_decode::{Decoder, SymphoniaAac, SymphoniaConfig};
//!
//! let decoder = Decoder::<SymphoniaAac>::new(file, SymphoniaConfig::default())?;
//! ```
//!
//! For runtime selection:
//! ```ignore
//! use kithara_decode::{DecoderFactory, CodecSelector, DecoderConfig};
//!
//! let decoder = DecoderFactory::create(file, CodecSelector::Auto, DecoderConfig::default())?;
//! ```

#![forbid(unsafe_code)]

mod decoder;
mod error;
mod factory;
mod symphonia;
mod traits;
mod types;

// Error types
pub use error::{DecodeError, DecodeResult};

// Core types
pub use types::{PcmChunk, PcmSpec, TrackMetadata};

// Traits and codec markers
pub use traits::{Aac, Alac, AudioDecoder, CodecType, Flac, Mp3, Vorbis};

// Generic wrapper
pub use decoder::Decoder;

// Symphonia backend
pub use symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};

// Factory for runtime selection
pub use factory::{CodecSelector, DecoderConfig, DecoderFactory, ProbeHint};

// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
