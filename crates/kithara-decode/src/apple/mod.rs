//! Apple `AudioToolbox` codec surface.
//!
//! Three pipelines share `AudioConverter` for PCM output:
//! - fMP4 AAC-LC / FLAC over HLS: container parsed by
//!   `crate::fmp4::Fmp4SegmentDemuxer`, frames decoded by [`AppleCodec`].
//! - Standalone MP3: frames parsed by the `kithara-mpa` demuxer, decoded by
//!   [`AppleCodec`].
//! - Standalone WAV / FLAC / AAC / ALAC: container parsed via
//!   `AudioFileServices` ([`file::AppleAudioFile`]), frames decoded by
//!   [`AppleCodec`].

pub(crate) mod codec;
pub(crate) mod consts;
pub(crate) mod converter;
pub(crate) mod demuxer;
pub(crate) mod file;
pub(crate) mod flac;

pub(crate) use codec::AppleCodec;
pub(crate) use converter::embedded_target_output_rate;
pub(crate) use demuxer::{AppleAudioFileDemuxer, SourceOpenMode};
