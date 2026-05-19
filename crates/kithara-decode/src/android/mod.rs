//! Android `MediaCodec` codec surface.
//!
//! Two pipelines share `AMediaCodec` for PCM output:
//! - fMP4 AAC-LC / FLAC over HLS: container parsed by
//!   `crate::fmp4::Fmp4SegmentDemuxer`, frames decoded by [`AndroidCodec`].
//! - Standalone WAV / MP3 / ALAC: container parsed via `AMediaExtractor`
//!   ([`media_extractor::AndroidMediaExtractor`]), frames decoded by
//!   [`AndroidCodec`]. No Symphonia required.

pub(crate) mod aformat;
pub(crate) mod codec;
pub(crate) mod error;
pub(crate) mod ffi;
mod jni;
pub(crate) mod media_codec;
#[cfg(target_os = "android")]
pub(crate) mod media_extractor;
#[cfg(target_os = "android")]
pub(crate) mod media_extractor_demuxer;

pub(crate) use codec::AndroidCodec;
pub(crate) use jni::ensure_current_thread_attached;
#[cfg(target_os = "android")]
pub(crate) use media_extractor_demuxer::AndroidMediaExtractorDemuxer;
