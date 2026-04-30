//! [`Demuxer`] trait definition.

use std::time::Duration;

use super::types::{DemuxOutcome, DemuxSeekOutcome, TrackInfo};
use crate::error::DecodeResult;

/// Container-side demuxer trait.
///
/// Implementations parse a container (HLS-fmp4, file-mp4, MP3, OGG, …)
/// and emit raw codec frames with timing metadata. The codec layer
/// ([`crate::FrameCodec`], introduced in a later phase) consumes those
/// frames into PCM.
pub trait Demuxer: Send {
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
