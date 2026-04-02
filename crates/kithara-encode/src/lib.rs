#![forbid(unsafe_code)]

//! # Kithara Encode
//!
//! Audio encoding library with a thin facade and FFmpeg-backed implementations.
//!
//! Use [`EncoderFactory`] for runtime codec selection:
//! ```ignore
//! use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
//!
//! let encoder = EncoderFactory::create_bytes(BytesEncodeTarget::Mp3)?;
//! let encoded = encoder.encode_bytes(BytesEncodeRequest {
//!     pcm: &pcm_source,
//!     target: BytesEncodeTarget::Mp3,
//!     bit_rate: None,
//! })?;
//! ```

mod error;
mod factory;
mod traits;
mod types;

#[cfg(not(target_arch = "wasm32"))]
mod ffmpeg;

pub use error::{EncodeError, EncodeResult};
pub use factory::EncoderFactory;
pub use traits::InnerEncoder;
pub use types::{
    BytesEncodeRequest, BytesEncodeTarget, EncodedAccessUnit, EncodedBytes, EncodedTrack,
    PackagedEncodeRequest, PcmSource,
};
