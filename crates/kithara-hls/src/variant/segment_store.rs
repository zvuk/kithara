use std::ops::Range;

use kithara_assets::{AssetScope, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Duration;
use kithara_stream::StreamResult;

use crate::{
    handle::ResourceHandle,
    segment::{MediaSegment, PlannedFetch, Segment},
};

/// Owns the per-variant segment/init content domain: the asset scope plus
/// the init slot and media [`Segment`] table. It vends a narrow
/// [`ResourceHandle`] per segment/init
/// ([`segment_handle`](Self::segment_handle) / [`init_handle`](Self::init_handle))
/// and routes the produce-core's per-slot reads/`contains` through the
/// [`Segment`] cascade — every disk read or acquire goes through the segment
/// (and its handle) rather than the scope directly, keeping the open-and-read
/// on one type — the home for the `WS5d` held-resource lease.
///
/// The init slot is `Some(Segment::Init)` for a variant that advertises a
/// separately fetched `#EXT-X-MAP` init, `None` otherwise (the old
/// `VariantInit::{Pending, NotApplicable}` discriminant, subsumed by the
/// [`Segment`] enum). Its existence is keyed on the playlist `#EXT-X-MAP`
/// URL, never on the init HEAD size — see [`HlsVariant::build_init_entry`](
/// super::HlsVariant::build_init_entry).
///
/// Coordinate geometry (offsets, shift, served range) lives in the sibling
/// [`Layout`](super::layout::Layout); read methods that bridge bytes and
/// content (`read_at`, `range_ready`, the descriptors) stay on the
/// `HlsVariant` facade and orchestrate across both.
pub(super) struct SegmentStore {
    /// Per-track asset store. The single source-of-truth clone: each vended
    /// [`ResourceHandle`] gets a cheap clone of it, and the fetch path
    /// acquires *writable* resources via the same clone in `PlanCtx::scope`.
    scope: AssetScope<DecryptContext>,
    init: Option<Segment>,
    segments: Vec<Segment>,
}

impl SegmentStore {
    pub(super) fn new(
        scope: AssetScope<DecryptContext>,
        init: Option<Segment>,
        segments: Vec<Segment>,
    ) -> Self {
        Self {
            scope,
            init,
            segments,
        }
    }

    /// Settle-side size store: shrink the appropriate atom to `final_len`.
    /// The caller runs this inside [`Layout::apply_commit`](
    /// super::layout::Layout::apply_commit)'s write-lock so a reader never
    /// observes a new size against a stale offset table.
    pub(super) fn apply_loaded_size(&self, planned: PlannedFetch, final_len: u64) {
        match planned {
            PlannedFetch::Init => {
                // Only a `Some(Init)` slot is ever settled (it is the only init
                // that gets fetched). A `None` init has no size atom; a stray
                // settle is a no-op rather than resurrecting an init.
                if let Some(init) = self.init.as_ref() {
                    init.size().set_exact(final_len);
                }
            }
            PlannedFetch::Segment(idx) => {
                if let Some(slot) = self.segments.get(idx as usize) {
                    slot.size().set_exact(final_len);
                }
            }
        }
    }

    /// Committed on-disk length for media segment `seg_idx` when its resource
    /// is `Committed` with a known `final_len` — the skip-fetch guard's size
    /// source (mirrors what `FetchSlot::settle` would have applied on a
    /// cache-hot resource). `None` when the index is out of range.
    pub(super) fn committed_final_len(&self, seg_idx: u32) -> Option<u64> {
        self.segment_handle(seg_idx)?.committed_len()
    }

    /// Whether every byte in `range` is already present on disk for media
    /// segment `seg_idx` — routed through the [`Segment`] cascade.
    pub(super) fn segment_contains(&self, seg_idx: u32, range: Range<u64>) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|seg| seg.contains(&self.scope, range))
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
        self.segments
            .get(seg_idx as usize)
            .map_or_else(|| Ok(None), |seg| seg.read_at(&self.scope, range, dst))
    }

    /// Whether every byte in `range` is present on disk for the init segment.
    pub(super) fn init_contains(&self, range: Range<u64>) -> bool {
        self.init
            .as_ref()
            .is_some_and(|seg| seg.contains(&self.scope, range))
    }

    /// Read `range` of the init segment into `dst` via the [`Segment`]
    /// cascade. `Ok(None)` when there is no init or its bytes are not on disk
    /// yet.
    pub(super) fn init_read_at(
        &self,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        self.init
            .as_ref()
            .map_or_else(|| Ok(None), |seg| seg.read_at(&self.scope, range, dst))
    }

    /// Index of the first non-`Loaded` segment — the ABR controller's
    /// "download head". Returns `num_segments()` when every segment is
    /// `Loaded`.
    pub(super) fn download_head(&self) -> u32 {
        let head = self
            .segments
            .iter()
            .position(|s| !s.state().is_loaded())
            .unwrap_or(self.segments.len());
        u32::try_from(head).unwrap_or(u32::MAX)
    }

    /// Whether the variant declares a separately fetched `#EXT-X-MAP` init —
    /// true regardless of whether its size is yet known. A `Some(Init)` slot
    /// with `init_size() == 0` (failed/absent HEAD, pre-commit) still counts:
    /// the init prefix `[0, init_size)` is reserved for it, not for media.
    pub(super) fn has_init(&self) -> bool {
        self.init.is_some()
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

    /// Borrow the init slot — the fetch path (on the facade) matches on it,
    /// reading the init `Segment`'s `url` / `content` / `resource_id` and
    /// claiming its state atom. `None` for a variant with no separate init.
    pub(super) fn init(&self) -> Option<&Segment> {
        self.init.as_ref()
    }

    /// Committed on-disk length of the (separately fetched) init segment, as
    /// [`committed_final_len`](Self::committed_final_len) for media.
    pub(super) fn init_committed_final_len(&self) -> Option<u64> {
        self.init_handle()?.committed_len()
    }

    pub(super) fn init_downloading(&self) -> bool {
        self.init
            .as_ref()
            .is_some_and(|seg| seg.state().is_downloading())
    }

    /// Whether the (separately fetched) init segment settled terminally.
    pub(super) fn init_failed(&self) -> bool {
        self.init
            .as_ref()
            .is_some_and(|seg| seg.state().is_failed())
    }

    /// Narrow disk handle for the variant's separately fetched init segment,
    /// or `None` for a variant with no `#EXT-X-MAP` init.
    pub(super) fn init_handle(&self) -> Option<ResourceHandle> {
        Some(self.init.as_ref()?.resource(&self.scope))
    }

    /// Resource key for the variant's init segment — `None` when the
    /// variant has no separately fetched init (raw TS/AAC, or byte-range
    /// embedded). Test-only; reads go through [`init_handle`](Self::init_handle).
    #[cfg(test)]
    pub(super) fn init_resource(&self) -> Option<ResourceKey> {
        Some(self.init.as_ref()?.resource_id().clone())
    }

    pub(super) fn init_size(&self) -> u64 {
        self.init.as_ref().map_or(0, Segment::len)
    }

    /// Flip the init slot to `Missing`; returns whether the variant
    /// actually carries an init segment, so the caller knows to clear the
    /// Layout seed in step.
    pub(super) fn invalidate_init(&self) -> bool {
        self.init.as_ref().is_some_and(|seg| {
            seg.state().mark_missing();
            true
        })
    }

    /// Whether the next dispatch should issue the separate init fetch —
    /// true only for a present, not-yet-loaded init segment.
    pub(super) fn needs_init_fetch(&self) -> bool {
        self.init
            .as_ref()
            .is_some_and(|seg| !seg.state().is_loaded())
    }

    #[must_use]
    pub(super) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` does
    /// not belong to this variant. Flips `Loaded -> Missing`; queue
    /// reseeding is the caller's job.
    pub(super) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if let Some(init) = self.init.as_ref()
            && init.resource_id() == key
        {
            init.state().mark_missing();
            return Some(-1);
        }
        for (seg_idx, seg) in self.segments.iter().enumerate() {
            if seg.resource_id() == key {
                seg.state().mark_missing();
                return i32::try_from(seg_idx).ok();
            }
        }
        None
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
        Some(self.segments.get(seg_idx as usize)?.resource(&self.scope))
    }

    /// Media size of segment `idx` — pure media only; the init prefix lives
    /// in its own `[0, init.size)` range.
    pub(super) fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.len())
    }

    /// Borrow the media table — the fetch path and the descriptor builders
    /// (on the facade) index it for `url` / `content` / `decode_time`.
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
