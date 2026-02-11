//! # Kithara Decode
//!
//! Audio decoding library with pluggable backends.
//!
//! Provides generic decoder infrastructure supporting Symphonia (software),
//! Apple `AudioToolbox`, and Android `MediaCodec` backends.
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

// NOTE: deny instead of forbid to allow unsafe in platform-specific FFI modules (apple, android)
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod decoder;
mod error;
mod factory;
mod symphonia;
mod traits;
mod types;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

// Platform-specific backends
#[cfg(all(feature = "android", target_os = "android"))]
mod android;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;

// Android MediaCodec backend (Android only)
#[doc(hidden)]
#[cfg(all(feature = "android", target_os = "android"))]
pub use android::{Android, AndroidAac, AndroidAlac, AndroidConfig, AndroidFlac, AndroidMp3};
// Apple AudioToolbox backend (macOS/iOS only)
#[doc(hidden)]
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub use apple::{Apple, AppleAac, AppleAlac, AppleConfig, AppleFlac, AppleMp3};
// Generic wrapper
#[doc(hidden)]
pub use decoder::Decoder;
// Error types
pub use error::{DecodeError, DecodeResult};
// Factory for runtime selection
#[doc(hidden)]
pub use factory::{CodecSelector, ProbeHint};
pub use factory::{DecoderConfig, DecoderFactory};
// Re-export types from kithara-stream for convenience
pub use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
// Symphonia backend
#[doc(hidden)]
pub use symphonia::{
    Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
};
// Test utilities
#[cfg(any(test, feature = "test-utils"))]
pub use traits::AudioDecoderMock;
// Public traits
pub use traits::InnerDecoder;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::InnerDecoderMock;
// Internal traits and codec markers
#[doc(hidden)]
pub use traits::{Aac, Alac, AudioDecoder, CodecType, DecoderInput, Flac, Mp3, Vorbis};
// Core types
pub use types::{PcmChunk, PcmSpec, TrackMetadata};
#[cfg(any(test, feature = "test-utils"))]
pub use unimock;
