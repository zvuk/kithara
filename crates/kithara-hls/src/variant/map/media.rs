use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_stream::{MediaInfo, StreamResult};

use super::HlsVariant;
use crate::{
    handle::ResourceHandle,
    segment::{MediaSegment, Segment},
};

impl HlsVariant {
    pub(crate) fn media_info(&self) -> MediaInfo {
        let variant_u32 = u32::try_from(self.variant).unwrap_or(u32::MAX);
        MediaInfo::builder()
            .maybe_codec(self.profile.codec)
            .maybe_container(self.profile.container)
            .variant_index(variant_u32)
            .build()
    }

    /// Committed on-disk length for media segment `seg_idx` when its resource
    /// is `Committed` with a known `final_len` — the skip-fetch guard's size
    /// source. `None` when the index is out of range.
    pub(super) fn committed_final_len(&self, seg_idx: u32) -> Option<u64> {
        self.segments
            .get(seg_idx as usize)?
            .committed_len(&self.segments.scope)
    }

    /// Whether every byte in `range` is already present on disk for media
    /// segment `seg_idx` — routed through the [`Segment`] cascade.
    pub(super) fn segment_contains(&self, seg_idx: u32, range: Range<u64>) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|seg| seg.size().is_exact() && seg.contains(&self.segments.scope, range))
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

    /// Clamped segment index whose decode-time window contains `t`, or
    /// `None` when the variant has no segments.
    pub(super) fn index_at_time(&self, t: Duration) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        Some(idx.min(self.segments.len() - 1))
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

    /// Media size of segment `idx` — pure media only; the init prefix lives
    /// in its own `[0, init.size)` range.
    pub(super) fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.read_len())
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
