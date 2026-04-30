#![forbid(unsafe_code)]

//! Segment-aware metadata accessor.
//!
//! [`SegmentedSource`] is a sidecar contract for sources that carry
//! per-segment byte-range and decode-time information (HLS). Decoders
//! that demux segment-by-segment — bypassing whole-stream container
//! parsers — query this trait to map a target time to a single segment
//! byte range without reading any prefix bytes.
//!
//! Decoupled from [`Source`](crate::Source) intentionally: `Source`
//! has an associated `Error` type which makes `dyn Source` impossible.
//! `SegmentedSource` is object-safe so it can be passed as
//! `Arc<dyn SegmentedSource>` into the decoder factory.
//!
//! Sources opt in via [`Source::as_segmented`](crate::Source::as_segmented).

use std::{ops::Range, sync::Arc};

use kithara_platform::time::Duration;

/// Per-segment metadata exposed by segmented sources (HLS).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentDescriptor {
    /// Byte range in the source's virtual stream.
    pub byte_range: Range<u64>,
    /// Absolute decode time at the start of this segment (cumulative
    /// EXTINF over preceding segments).
    pub decode_time: Duration,
    /// Segment duration (EXTINF).
    pub duration: Duration,
    /// Segment index within the variant.
    pub segment_index: u32,
    /// Variant the descriptor was resolved against.
    pub variant_index: usize,
}

impl SegmentDescriptor {
    #[must_use]
    pub fn new(
        byte_range: Range<u64>,
        decode_time: Duration,
        duration: Duration,
        segment_index: u32,
        variant_index: usize,
    ) -> Self {
        Self {
            byte_range,
            decode_time,
            duration,
            segment_index,
            variant_index,
        }
    }
}

/// Object-safe accessor for segment-level metadata.
///
/// Used by segment-aware decoders (e.g. fMP4 segment demuxer) to map
/// time/byte targets to a single segment's byte range without walking
/// the whole stream. Sources that do not have segments do not implement
/// this trait.
pub trait SegmentedSource: Send + Sync + 'static {
    /// Init segment range (e.g. ftyp+moov from `EXT-X-MAP`) for the
    /// current layout variant. Returns `None` until the init segment
    /// is announced.
    fn init_segment_range(&self) -> Option<Range<u64>>;

    /// Locate the segment whose
    /// `[decode_time, decode_time + duration)` covers `t`.
    ///
    /// Resolves against the source's *current layout variant* — same
    /// variant `init_segment_range` describes — so that decoder seeks
    /// stay within one byte space.
    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor>;

    /// Next segment whose byte range starts at or after `byte_offset`.
    /// Used for sequential play after the current segment is consumed.
    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor>;

    /// Total number of segments in the current layout variant.
    fn segment_count(&self) -> Option<u32>;
}

/// Type alias: shared handle threaded into the decoder factory.
pub type SharedSegmentedSource = Arc<dyn SegmentedSource>;
