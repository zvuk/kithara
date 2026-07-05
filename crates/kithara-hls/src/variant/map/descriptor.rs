use std::{io::Error as IoError, ops::Range};

use kithara_platform::time::Duration;
use kithara_stream::{SegmentDescriptor, SourceError, StreamError, StreamResult};
use kithara_test_utils::kithara;

use super::HlsVariant;

impl HlsVariant {
    /// Segment that owns fetch demand for `byte_offset`.
    ///
    /// `find_at_offset` is intentionally media-only: bytes inside an fMP4 init
    /// prefix are not media bytes. The peer planner asks a different question:
    /// if the reader is parked in the active init prefix, segment 0 is the
    /// fetch plan that must carry the init + first media bytes needed by a
    /// decoder recreate.
    pub(crate) fn demand_segment_at_offset(&self, byte_offset: u64) -> Option<u32> {
        if self.init_descriptor_at(byte_offset).is_some() && !self.segments.is_empty() {
            return Some(0);
        }
        self.find_at_offset(byte_offset).map(|(idx, _, _)| idx)
    }

    pub(crate) fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
        let entry = self.segments.get(idx)?.as_media()?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        let byte_offset = self.layout.natural_offset(idx)?;
        let size = self.segment_size(idx)?;
        Some(SegmentDescriptor::new(
            byte_offset..byte_offset + size,
            entry.decode_time(),
            entry.duration(),
            seg_idx_u32,
            self.variant,
        ))
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let mut idx = self.layout.bisect_left(byte);
        if idx >= self.segments.len() {
            return None;
        }
        let off = self.layout.natural_offset(idx)?;
        if off < byte {
            idx += 1;
        }
        if idx >= self.segments.len() {
            return None;
        }
        self.descriptor(idx)
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_at_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let (idx, off, size) = self.find_at_offset(byte)?;
        let entry = self.segments.get(idx as usize)?.as_media()?;
        Some(SegmentDescriptor::new(
            off..off + size,
            entry.decode_time(),
            entry.duration(),
            idx,
            self.variant,
        ))
    }

    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn descriptor_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.descriptor(self.index_at_time(t)?)
    }

    /// Header byte range for format-change resync — alias for
    /// [`Self::header_byte_range`] under the `Source` trait's name.
    pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.header_byte_range()
    }

    /// Byte range a demuxer reads to re-establish container state after a
    /// format change (variant flip or codec change).
    ///
    /// `Ok(init_range)` for `served_from() == 0`, else
    /// `Err(FormatChangeNotApplicable)` for byte-shifted same-codec
    /// commits. See the crate `CONTEXT.md` "Format-change header byte range".
    pub(crate) fn header_byte_range(&self) -> StreamResult<Range<u64>> {
        if self.served_from() != 0 {
            return Err(StreamError::Source(SourceError::FormatChangeNotApplicable));
        }
        if self.init().is_some() {
            return Ok(self.init_byte_range());
        }
        let (_, off, size) = self.layout.find_natural(0, &self.segments).ok_or_else(|| {
            StreamError::Source(SourceError::Other(Box::new(IoError::other(
                "variant has no segments — cannot derive header range",
            ))))
        })?;
        Ok(off..(off + size))
    }

    /// Addressable init prefix range for the byte at `byte_offset`. Returns
    /// `None` when the byte falls outside this variant's *virtually
    /// addressable* init range — the init only counts when
    /// `served_from() == 0` (post-commit, init is orphaned in natural
    /// space). Reads/`contains` for that range route through the init
    /// [`Segment`] ([`Self::init_read_at`] / [`Self::init_contains`]).
    pub(crate) fn init_descriptor_at(&self, byte_offset: u64) -> Option<Range<u64>> {
        if self.served_from() != 0 || !self.has_init() {
            return None;
        }
        let range = self.init_byte_range();
        if range.is_empty() || !range.contains(&byte_offset) {
            return None;
        }
        Some(range)
    }
}
