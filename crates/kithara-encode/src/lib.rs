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
#[cfg(not(target_arch = "wasm32"))]
mod offline;
#[cfg(not(target_arch = "wasm32"))]
mod stream;
#[cfg(test)]
mod test_pcm;
mod traits;
mod types;

#[cfg(all(not(target_arch = "wasm32"), feature = "fdk-aac"))]
mod fdk;
#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
mod ffmpeg;

pub use error::{EncodeError, EncodeResult};
pub use factory::EncoderFactory;
#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
pub use ffmpeg::flac::normalize_flac_codec_config;
#[cfg(not(target_arch = "wasm32"))]
pub use stream::{StreamBackend, StreamEncoder};
pub use traits::InnerEncoder;
pub use types::{
    BytesEncodeRequest, BytesEncodeTarget, EncodedAccessUnit, EncodedBytes, EncodedTrack,
    PackagedEncodeRequest, PcmSource,
};
