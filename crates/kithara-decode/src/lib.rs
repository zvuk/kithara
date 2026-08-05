// NOTE: deny instead of forbid to allow unsafe in Android FFI modules.
#![deny(unsafe_code)]

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
mod composed;
mod demuxer;
mod error;
mod factory;
mod fmp4;
mod gapless;
mod mp4;
mod pcm;
mod resampled;
#[cfg(feature = "symphonia")]
mod symphonia;
mod traits;
mod types;
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
mod webcodecs;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

#[cfg(all(feature = "android", target_os = "android"))]
mod android;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;

pub use codec::CodecPriming;
pub use error::{DecodeError, DecodeResult, ErrorClass};
pub use factory::{DecoderBackend, DecoderConfig, DecoderFactory, DecoderResamplerConfig};
pub use gapless::{
    GaplessInfo, GaplessMode, GaplessOutput, GaplessTailCompensation, GaplessTrimmer,
    SilenceTrimParams, probe_mp4_gapless,
};
pub use pcm::{ChunkSink, DropChunks, duration_for_frames, frames_for_duration, sanitize_sample};
pub use traits::{
    Decoder, DecoderChunkOutcome, DecoderInput, DecoderSeekOutcome, InputReadOutcome,
};
pub use types::{
    BlenderProfile, DecoderTrackInfo, GaplessProfile, PcmChunk, PcmMeta, PcmSpec, TrackMetadata,
};
#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
pub use webcodecs::probe::spawn_webcodecs_probe;
