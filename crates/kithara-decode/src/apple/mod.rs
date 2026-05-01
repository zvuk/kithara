//! Apple `AudioToolbox` codec surface.
//!
//! `AppleCodec` (`FrameCodec` impl over `AudioConverter`) consumes already-demuxed
//! frames; container parsing is the demuxer's job. HW-acceleration preserved for
//! AAC-LC / FLAC fMP4. Non-fmp4 file decoding (MP3, raw FLAC, WAV) flows through
//! the Symphonia software path.

pub(crate) mod codec;
pub(crate) mod consts;
pub(crate) mod converter;
pub(crate) mod ffi;

pub(crate) use codec::AppleCodec;
