//! `Demuxer` trait — container-side half of the unified decoder architecture.
//!
//! A `Demuxer` consumes raw bytes from a `Source`-backed transport and
//! yields per-frame slices with PTS / duration. It does NOT touch PCM —
//! that is the [`FrameCodec`] half. Splitting the two lets us pair any
//! container parser (HLS-fmp4, file-mp4, ADTS, OGG, …) with any codec
//! backend (Symphonia software, Apple/Android hardware, …) through
//! `UniversalDecoder<D, C>`.
//!
//! Implementations live in their platform-specific modules:
//! - [`crate::symphonia::SymphoniaDemuxer`] for MP3 / WAV / OGG / native FLAC / file-fmp4.
//! - [`crate::fmp4::Fmp4SegmentDemuxer`] for HLS fMP4 (segment-aware).
//!
//! [`FrameCodec`]: crate::codec::FrameCodec

use std::time::Duration;

use kithara_stream::{AudioCodec, PendingReason};

use crate::error::DecodeResult;

/// Container-side demuxer trait.
///
/// Implementations parse a container (HLS-fmp4, file-mp4, MP3, OGG, …)
/// and emit raw codec frames with timing metadata. The codec layer
/// ([`crate::codec::FrameCodec`]) consumes those frames into PCM.
pub(crate) trait Demuxer: Send {
    /// Track-level metadata exposed by the container.
    fn track_info(&self) -> &TrackInfo;

    /// Total duration if the container can compute one (HLS playlist
    /// total, mp4 `mvhd`, …); `None` for live or unbounded streams.
    fn duration(&self) -> Option<Duration>;

    /// Pull the next demuxed frame.
    ///
    /// # Errors
    ///
    /// Surfaces parser-level failures verbatim. Source-level pending
    /// states return `Ok(DemuxOutcome::Pending(_))`.
    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome>;

    /// Seek the demuxer to `target` time.
    ///
    /// Returns the actual landing point — `Landed { landed_at }` for a
    /// successful seek, `PastEof { duration }` when the target lies
    /// beyond the stream's known length.
    ///
    /// # Errors
    ///
    /// Surfaces parser-level seek failures verbatim.
    fn seek(&mut self, target: Duration) -> DecodeResult<DemuxSeekOutcome>;
}

/// Track-level metadata produced by [`Demuxer::track_info`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(crate) struct TrackInfo {
    /// Audio codec carried by this track.
    pub codec: AudioCodec,
    /// Decoded sample rate (Hz).
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Codec-specific extra data — `AudioSpecificConfig` (AAC),
    /// `STREAMINFO` (FLAC), `esds` cookie (Apple), etc. Empty when the
    /// codec needs no extra data.
    pub extra_data: Vec<u8>,
    /// Total track duration if available.
    pub duration: Option<Duration>,
}

/// One demuxed audio frame.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct Frame {
    /// Raw frame bytes — owned so the caller can reuse / queue them
    /// without holding the demuxer's internal buffer.
    pub data: Vec<u8>,
    /// Presentation time of this frame.
    pub pts: Duration,
    /// Frame duration.
    pub duration: Duration,
}

/// Result of a [`Demuxer::next_frame`] call.
#[derive(Debug)]
pub(crate) enum DemuxOutcome {
    /// One frame demuxed. Caller routes it to the codec layer.
    Frame(Frame),
    /// No frame available right now — caller should re-poll later.
    Pending(PendingReason),
    /// Natural end of stream.
    Eof,
}

/// Result of a [`Demuxer::seek`] call.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub(crate) enum DemuxSeekOutcome {
    /// Successfully landed inside the stream. `landed_at` is the
    /// authoritative target (≤ requested target). `landed_byte` is the
    /// optional byte-level cursor for source-level reconciliation.
    Landed {
        landed_at: Duration,
        landed_byte: Option<u64>,
    },
    /// The seek target lies past the stream's end; `duration` is the
    /// total stream duration.
    PastEof { duration: Duration },
}
