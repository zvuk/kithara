//! Segment-by-segment fMP4 demuxer + decoder.
//!
//! Mirrors the `ExoPlayer` / hls.js architecture: each HLS segment is
//! parsed independently against a cached `Fmp4InitInfo` extracted from
//! the variant's `EXT-X-MAP` init segment. Audio frames are demuxed
//! straight from `moof`/`mdat` byte ranges and fed to a frame-level
//! codec (Symphonia AAC/FLAC, Apple `AudioConverter`).
//!
//! Bypasses the whole-stream container parser entirely — Symphonia's
//! `IsoMp4Reader::try_new` walks every `moof+mdat` fragment for
//! sidx-less fMP4, which is the root cause of the HLS seek-skip freeze.

pub(crate) mod demux;
pub(crate) mod source_io;

#[cfg(all(test, feature = "symphonia"))]
mod tests;
