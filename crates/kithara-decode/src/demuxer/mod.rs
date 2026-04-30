//! Public `Demuxer` trait — the container-side half of the unified
//! decoder architecture.
//!
//! A `Demuxer` consumes raw bytes from a `Source`-backed transport and
//! yields per-frame slices with PTS / duration. It does NOT touch PCM —
//! that is the [`crate::FrameCodec`] half. Splitting the two lets us
//! pair any container parser (HLS-fmp4, file-mp4, ADTS, OGG, …) with any
//! codec backend (Symphonia software, Apple/Android hardware, …) through
//! `UniversalDecoder<D, C>`.
//!
//! Implementations are added incrementally in subsequent phases:
//! - `SymphoniaDemuxer` adapter wraps `Box<dyn FormatReader>` for MP3 /
//!   WAV / OGG / native FLAC / file-fmp4.
//! - `Fmp4SegmentDemuxer` wraps the existing segment-aware `re_mp4` path
//!   for HLS-fmp4.
//!
//! The trait + types live here so Phase 4's `FrameCodec` peer trait and
//! `UniversalDecoder<D, C>` can name them without import cycles.

mod contract;
#[cfg(feature = "symphonia")]
mod symphonia;
mod types;

pub use contract::Demuxer;
#[cfg(feature = "symphonia")]
pub use symphonia::SymphoniaDemuxer;
pub use types::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo};
