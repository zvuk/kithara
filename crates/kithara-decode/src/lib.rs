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

mod codec;
mod demuxer;
mod error;
mod factory;
mod fmp4;
mod hooks;
mod pcm;
#[cfg(feature = "symphonia")]
mod symphonia;
mod traits;
mod types;
mod universal;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

// Platform-specific backends. Gated once here; no internal cfg attrs.
#[cfg(all(feature = "android", target_os = "android"))]
mod android;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;

pub use error::{DecodeError, DecodeResult};
// Factory for runtime selection
pub use factory::{DecoderBackend, DecoderConfig, DecoderFactory};
// Reader-side hook wrapper (the trait + signals live in `kithara-stream`).
pub use hooks::HookedDecoder;
// Public traits
pub use traits::{
    Decoder, DecoderChunkOutcome, DecoderInput, DecoderSeekOutcome, InputReadOutcome,
};
// Core types
pub use types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
