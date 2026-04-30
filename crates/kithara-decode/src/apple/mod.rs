//! Apple `AudioToolbox` FFI surface, reused by [`crate::codec::AppleCodec`].
//!
//! The legacy god-type `AppleDecoder` (which combined `AudioFile`
//! container parsing and `AudioConverter` codec decoding) was removed in
//! favour of the unified path: `Fmp4SegmentDemuxer` drives container
//! parsing, `AppleCodec` drives codec decoding via `AudioConverter`. The
//! HW-accelerated path is preserved for AAC / FLAC fMP4; non-fmp4 file
//! decoding (MP3, raw FLAC, WAV) flows through the Symphonia software
//! path even when the user selects [`crate::DecoderBackend::Apple`].

pub(crate) mod consts;
pub(crate) mod converter;
pub(crate) mod ffi;
