//! # Kithara Encode
//!
//! Portable PCM/WAV sessions and optional native audio encoders.
//!
//! Use [`EncodeConfig`], [`EncoderSession`], and [`ContainerSession`] for a
//! continuous output, or [`EncoderFactory`] for finite native encoding:
//! ```ignore
//! use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
//!
//! let encoded = EncoderFactory::encode_bytes(&BytesEncodeRequest {
//!     pcm: &pcm_source,
//!     target: BytesEncodeTarget::Mp3,
//!     bit_rate: None,
//! })?;
//! ```

mod config;
mod error;
mod factory;
#[cfg(not(target_arch = "wasm32"))]
mod offline;
mod session;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
mod stream;
#[cfg(test)]
mod test_pcm;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod types;

#[cfg(all(not(target_arch = "wasm32"), feature = "fdk-aac"))]
mod fdk;
#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
mod ffmpeg;

pub use config::EncodeConfig;
pub use error::{EncodeError, EncodeResult};
pub use factory::EncoderFactory;
#[cfg(all(not(target_arch = "wasm32"), feature = "ffmpeg"))]
pub use ffmpeg::flac::normalize_flac_codec_config;
pub use session::{ContainerFinish, ContainerSession, ContainerWrite, EncoderSession};
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "ffmpeg", feature = "fdk-aac")
))]
pub use stream::{StreamBackend, StreamEncoder};
pub use types::{
    BytesEncodeRequest, BytesEncodeTarget, EncodedAccessUnit, EncodedBytes, EncodedTrack,
    PackagedEncodeRequest, PcmSource,
};
mod consts;
