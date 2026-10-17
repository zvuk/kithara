use std::ops::Range;

use kithara_platform::time::Duration;

use crate::{SegmentDescriptor, SourceSeekAnchor, StreamResult};

/// Segment-table view exposed by segmented sources (HLS, fragmented
/// file-mp4).
///
/// Carries the segment metadata that segment-aware decoders need to
/// route reads — `init_segment_range` (ftyp+moov / `EXT-X-MAP`),
/// `segment_at_time`, `segment_after_byte`, `segment_count`, and total
/// `len`. Has no I/O surface: the byte cursor is the decoder's
/// `Read + Seek` handle, queried independently. Sources that aren't
/// segment-aware return `None` from [`crate::Source::byte_map`].
pub trait ByteMap: Send + Sync + 'static {
    /// Resolve `position` to a source-specific seek anchor (segment
    /// boundary + byte offset). Non-segmented / non-anchoring layouts
    /// keep the default `Ok(None)`.
    ///
    /// # Errors
    /// Returns an error when the layout cannot resolve the anchor.
    fn anchor_at_time(&self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(None)
    }

    /// Init segment range (e.g. ftyp+moov from `EXT-X-MAP`) for the
    /// current layout variant. Returns an **empty** range (`0..0`) when
    /// the layout has no init segment (raw TS/AAC/MPEG-ES) or when the
    /// active variant has not yet announced one. Callers that require an
    /// init must check `Range::is_empty()` — distinguishing "no init"
    /// from "init at offset 0..0" is unsupported because every init we
    /// emit is non-empty by construction.
    fn init_segment_range(&self) -> Range<u64>;

    /// Whether the layout currently reports zero bytes. `len()` is `Option`
    /// because some segmented sources do not know their total upfront, so
    /// emptiness defaults to "len is `None` or `Some(0)`".
    fn is_empty(&self) -> bool {
        self.len().is_none_or(|n| n == 0)
    }

    /// Total byte length across all segments. Used to compute total
    /// duration when the source can't provide a direct value.
    fn len(&self) -> Option<u64>;

    /// Next segment whose byte range starts at or after `byte_offset`.
    /// Used for sequential play after the current segment is consumed.
    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor>;

    /// Segment whose `byte_range` covers `byte_offset`. Default `None`
    /// keeps non-segmented sources transparent.
    fn segment_at_byte(&self, _byte_offset: u64) -> Option<SegmentDescriptor> {
        None
    }

    /// Descriptor for the segment at `segment_index` in the current
    /// layout variant. Used by demuxers to re-resolve a cursor's
    /// `byte_range` against the live layout — without this, a DRM
    /// post-decrypt size shrink (PKCS7 padding stripped) between cursor
    /// setup and the actual read leaves `state.range` pointing past
    /// the segment's real end and `HlsSource::read_at` splices bytes
    /// from the next segment onto the buffer's tail. Returns `None`
    /// for non-segmented sources or for indices outside the current
    /// layout's range.
    fn segment_at_index(&self, _segment_index: u32) -> Option<SegmentDescriptor> {
        None
    }

    /// Locate the segment whose `[decode_time, decode_time + duration)`
    /// covers `t`. Resolves against the source's *current layout
    /// variant* — same variant `init_segment_range` describes.
    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor>;

    /// Total number of segments in the current layout variant.
    fn segment_count(&self) -> Option<u32>;
}
