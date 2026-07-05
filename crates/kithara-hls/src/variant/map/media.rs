use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_stream::{MediaInfo, StreamResult};

use super::HlsVariant;
use crate::{
    handle::ResourceHandle,
    segment::{MediaSegment, Segment},
};

impl HlsVariant {
    /// Committed on-disk length for media segment `seg_idx` when its resource
    /// is `Committed` with a known `final_len` — the skip-fetch guard's size
    /// source. `None` when the index is out of range.
    pub(super) fn committed_final_len(&self, seg_idx: u32) -> Option<u64> {
        self.segments
            .get(seg_idx as usize)?
            .committed_len(&self.segments.scope)
    }

    /// Clamped segment index whose decode-time window contains `t`, or
    /// `None` when the variant has no segments.
    pub(super) fn index_at_time(&self, t: Duration) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        Some(idx.min(self.segments.len() - 1))
    }

    pub(crate) fn media_info(&self) -> MediaInfo {
        let variant_u32 = u32::try_from(self.variant).unwrap_or(u32::MAX);
        MediaInfo::builder()
            .maybe_codec(self.profile.codec)
            .maybe_container(self.profile.container)
            .variant_index(variant_u32)
            .build()
    }

    /// Bisect the decode-time table for the segment whose window contains
    /// `t`, returning `(index, segment_start, segment_end)`. Replaces the
    /// former O(N) linear playlist duration scan on the hot seek path with
    /// the same `O(log N)` decode-time bisect the byte map already uses.
    /// `None` only when the variant has no segments.
    pub(crate) fn seek_point_at_time(&self, t: Duration) -> Option<(u32, Duration, Duration)> {
        let idx = self.index_at_time(t)?;
        let seg = self.segments.get(idx)?.as_media()?;
        let start = seg.decode_time();
        let end = start.saturating_add(seg.duration());
        Some((u32::try_from(idx).ok()?, start, end))
    }

    /// Whether every byte in `range` is already present on disk for media
    /// segment `seg_idx` — routed through the [`Segment`] cascade.
    pub(super) fn segment_contains(&self, seg_idx: u32, range: Range<u64>) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|seg| seg.size().is_exact() && seg.contains(&self.segments.scope, range))
    }

    pub(super) fn segment_downloading(&self, seg_idx: u32) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|s| s.state().is_downloading())
    }

    /// Whether media segment `seg_idx` settled terminally (`Failed`): the
    /// downloader exhausted its retry budget, so the segment will never
    /// load. Readers surface a terminal error on it.
    pub(super) fn segment_failed(&self, seg_idx: u32) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|s| s.state().is_failed())
    }

    /// Narrow disk handle for media segment `seg_idx`, or `None` when the
    /// index is out of range. Cheap: clones the shared scope plus the
    /// segment's key and url.
    pub(super) fn segment_handle(&self, seg_idx: u32) -> Option<ResourceHandle> {
        Some(
            self.segments
                .get(seg_idx as usize)?
                .resource(&self.segments.scope),
        )
    }

    /// Read `range` of media segment `seg_idx` into `dst` via the [`Segment`]
    /// cascade. `Ok(None)` when the segment is out of range or its bytes are
    /// not on disk yet.
    pub(super) fn segment_read_at(
        &self,
        seg_idx: u32,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        self.segments.get(seg_idx as usize).map_or_else(
            || Ok(None),
            |seg| {
                if seg.size().is_exact() {
                    seg.read_at(&self.segments.scope, range, dst)
                } else {
                    Ok(None)
                }
            },
        )
    }

    /// Media size of segment `idx` — pure media only; the init prefix lives
    /// in its own `[0, init.size)` range.
    pub(super) fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.read_len())
    }

    /// ABR stalled-escape gate: media segment `seg_idx` is the reader's
    /// current segment at a clean boundary (`reader_pos` sits exactly at the
    /// segment's start byte — nothing read past it), and its in-flight fetch
    /// has crossed the downloader's `soft_timeout` (`on_slow` fired) without
    /// settling. True ⇒ this variant cannot deliver the byte range the reader
    /// is parked on, so a pending switch is safe to commit off the boundary
    /// gate (no mid-segment splice).
    pub(crate) fn segment_stalled_at_boundary(&self, seg_idx: u32, reader_pos: u64) -> bool {
        self.segment_byte_offset(seg_idx) == Some(reader_pos)
            && self
                .segments
                .get(seg_idx as usize)
                .is_some_and(|s| s.state().is_slow() && s.state().is_downloading())
    }

    /// Borrow the media table — the fetch path and the descriptor builders
    /// index it for `url` / `content` / `decode_time`.
    #[cfg(test)]
    pub(super) fn segments(&self) -> &[Segment] {
        &self.segments
    }
}

fn bisect_right_decode_time(segments: &[Segment], t: Duration) -> usize {
    let mut lo = 0_usize;
    let mut hi = segments.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let decode_time = segments[mid]
            .as_media()
            .map_or(Duration::ZERO, MediaSegment::decode_time);
        if decode_time <= t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
