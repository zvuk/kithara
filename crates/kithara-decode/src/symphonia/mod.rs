//! Symphonia codec + demuxer surface.
//!
//! `SymphoniaCodec` (`FrameCodec` impl over `Box<dyn AudioDecoder>`) and
//! `SymphoniaDemuxer` (`Demuxer` impl over `Box<dyn FormatReader>`) are the
//! pieces that pair with `UniversalDecoder` for software-decoded audio
//! formats (MP3, native FLAC, OGG/Opus/Vorbis, WAV/AIFF, MKV, ADTS,
//! file-fmp4). Bootstrap helpers — `adapter::ReadSeekAdapter` (`Read+Seek ->
//! MediaSource` bridge), `probe::{new_direct, probe_with_seek}`, and
//! `echain` (error-chain inspection) — back both.

pub(crate) mod adapter;
pub(crate) mod codec;
pub(crate) mod config;
pub(crate) mod demuxer;
pub(crate) mod echain;
pub(crate) mod probe;

pub(crate) use codec::SymphoniaCodec;
pub(crate) use demuxer::SymphoniaDemuxer;

#[cfg(test)]
mod tests;
