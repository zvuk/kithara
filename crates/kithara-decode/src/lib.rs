// NOTE: deny instead of forbid to allow unsafe in platform-specific FFI modules (apple, android)
#![deny(unsafe_code)]
#![allow(clippy::ignored_unit_patterns)]
#![cfg_attr(test, allow(clippy::allow_attributes))]

//! # Kithara Decode
//!
//! Audio decoding library with pluggable backends.
//!
//! Provides generic decoder infrastructure supporting Symphonia (software),
//! Apple `AudioToolbox`, and Android `MediaCodec` backends.
//!
//! ## Usage
//!
//! Use [`DecoderFactory`] for runtime codec selection:
//! ```ignore
//! use kithara_decode::{DecoderFactory, DecoderConfig};
//!
//! let decoder = DecoderFactory::create_from_media_info(source, &media_info, config)?;
//! ```

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
// Error types
pub use error::{DecodeError, DecodeResult};
// Factory for runtime selection
pub use factory::{DecoderConfig, DecoderFactory};
// Symphonia backend
#[doc(hidden)]
pub use symphonia::{Symphonia, SymphoniaAac, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis};
// Public traits
pub use traits::InnerDecoder;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::InnerDecoderMock;
// Core types
pub use types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
#[cfg(any(test, feature = "test-utils"))]
pub use unimock;
