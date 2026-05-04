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
mod gapless;
mod mp4;
mod pcm;
mod pcm_time;
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

pub use error::{DecodeError, DecodeResult, ErrorClass};
// Factory for runtime selection
pub use factory::{DecoderBackend, DecoderConfig, DecoderFactory};
// Gapless API — wholesale port from PR #64. Per-platform codec wiring
// in P4–P6, factory wiring in P7, audio-pipeline wiring in P9.
pub use gapless::{
    GaplessInfo, GaplessMode, GaplessOutput, GaplessTrimmer, SilenceTrimParams,
    codec_priming_frames, probe_mp4_gapless,
};
pub use pcm_time::{duration_for_frames, frames_for_duration};
// Public traits
pub use traits::{
    Decoder, DecoderChunkOutcome, DecoderInput, DecoderSeekOutcome, InputReadOutcome,
};
// Core types
pub use types::{DecoderTrackInfo, PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
