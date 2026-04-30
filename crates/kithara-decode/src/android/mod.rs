//! Android `MediaCodec` FFI surface, reused by [`crate::codec::AndroidCodec`].
//!
//! The legacy god-type `AndroidDecoder` (which combined `AMediaExtractor`
//! container parsing and `AMediaCodec` codec decoding) was removed in
//! favour of the unified path: `Fmp4SegmentDemuxer` drives container
//! parsing, `AndroidCodec` drives codec decoding via `AMediaCodec`. The
//! HW-accelerated path is preserved for AAC / FLAC fMP4; non-fmp4 file
//! decoding falls through to the Symphonia software path.

pub(crate) mod aformat;
pub(crate) mod codec;
pub(crate) mod error;
pub(crate) mod ffi;
mod jni;

pub(crate) use jni::ensure_current_thread_attached;
