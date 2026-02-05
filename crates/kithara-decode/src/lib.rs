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

// Platform-specific backends
#[cfg(all(feature = "android", target_os = "android"))]
mod android;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;

// Error types
// Android MediaCodec backend (Android only)
#[cfg(all(feature = "android", target_os = "android"))]
pub use android::{Android, AndroidAac, AndroidAlac, AndroidConfig, AndroidFlac, AndroidMp3};
// Apple AudioToolbox backend (macOS/iOS only)
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub use apple::{Apple, AppleAac, AppleAlac, AppleConfig, AppleFlac, AppleMp3};
// Generic wrapper
pub use decoder::Decoder;
pub use error::{DecodeError, DecodeResult};
// Factory for runtime selection
pub use factory::{CodecSelector, DecoderConfig, DecoderFactory, ProbeHint};
// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
// Symphonia backend
pub use symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};
// Traits and codec markers
pub use traits::{Aac, Alac, AudioDecoder, CodecType, Flac, InnerDecoder, Mp3, Vorbis};
// Core types
pub use types::{PcmChunk, PcmSpec, TrackMetadata};
