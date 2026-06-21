//! Apple `AudioToolbox` codec surface.
//!
//! Two pipelines share `AudioConverter` for PCM output:
//! - fMP4 AAC-LC / FLAC over HLS: container parsed by
//!   `crate::fmp4::Fmp4SegmentDemuxer`, frames decoded by [`AppleCodec`].
//! - Standalone WAV / MP3 / ALAC: container parsed via `AudioFileServices`
//!   ([`audio_file::AppleAudioFile`]), frames decoded by [`AppleCodec`].
//!   No Symphonia required.

pub(crate) mod audio_file;
pub(crate) mod audio_file_demuxer;
pub(crate) mod codec;
pub(crate) mod consts;
pub(crate) mod converter;
pub(crate) mod ffi;

pub(crate) use audio_file_demuxer::{AppleAudioFileDemuxer, SourceOpenMode};
pub(crate) use codec::AppleCodec;
