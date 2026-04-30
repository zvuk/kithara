//! Public `FrameCodec` trait — codec-side peer of [`crate::Demuxer`].
//!
//! Implementations consume already-demuxed frames (raw codec bytes +
//! PTS) and produce interleaved f32 PCM. Concrete adapters
//! (`SymphoniaCodec`, `AppleCodec`, `AndroidCodec`) come in subsequent
//! phases; this module currently only owns the contract so other
//! crates can reference it.

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple;
mod contract;
#[cfg(feature = "symphonia")]
mod symphonia;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub(crate) use apple::AppleCodec;
pub use contract::{DecodedFrame, FrameCodec};
#[cfg(feature = "symphonia")]
pub use symphonia::SymphoniaCodec;
