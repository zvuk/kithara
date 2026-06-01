use std::{io::ErrorKind, ops::Range, sync::atomic::Ordering};

use kithara_assets::{AssetResourceState, AssetScope, AssetsError, ReadSide, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_platform::time::Duration;
use kithara_stream::{StreamError, StreamResult};

use super::{InitEntry, PlannedFetch, SegmentEntry};
use crate::HlsError;

/// Owns the per-variant segment/init content domain: the asset scope plus
/// the init and media [`SegmentEntry`] tables. Every resource read goes
/// through here ([`read_resource`](Self::read_resource) /
/// [`contains_range`](Self::contains_range)), keeping the produce-core's
/// open-and-read on one type — the home for the `WS5d` held-resource lease.
///
/// Coordinate geometry (offsets, shift, served range) lives in the sibling
/// [`Layout`](super::layout::Layout); read methods that bridge bytes and
/// content (`read_at`, `range_ready`, the descriptors) stay on the
/// `HlsVariant` facade and orchestrate across both.
pub(super) struct SegmentStore {
    /// Per-track asset store. Reader paths (`read_resource`,
    /// `contains_range`, `committed_final_len`) open and probe resources
    /// through it; the fetch path acquires *writable* resources via
    /// `PlanCtx::scope` (the same clone), so this field is read-only here.
    scope: AssetScope<DecryptContext>,
    init: InitEntry,
    segments: Vec<SegmentEntry>,
}

impl SegmentStore {
    pub(super) fn new(
        scope: AssetScope<DecryptContext>,
        init: InitEntry,
        segments: Vec<SegmentEntry>,
    ) -> Self {
        Self {
            scope,
            init,
            segments,
        }
    }

    /// Borrow the init entry — the fetch path (on the facade) reads its
    /// `url` / `content` / `resource_id` and claims its state atom.
    pub(super) fn init(&self) -> &InitEntry {
        &self.init
    }

    /// Borrow the media table — the fetch path and the descriptor builders
    /// (on the facade) index it for `url` / `content` / `decode_time`.
    pub(super) fn segments(&self) -> &[SegmentEntry] {
        &self.segments
    }

    #[must_use]
    pub(super) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    pub(super) fn init_size(&self) -> u64 {
        self.init.size.load(Ordering::Acquire)
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC).
    pub(super) fn init_resource(&self) -> Option<ResourceKey> {
        (self.init_size() > 0).then(|| self.init.resource_id.clone())
    }

    /// Whether the next dispatch should issue the separate init fetch —
    /// true only for a non-zero, not-yet-loaded init segment.
    pub(super) fn needs_init_fetch(&self) -> bool {
        self.init_size() > 0 && !self.init.state.is_loaded()
    }

    /// Flip the init slot to `Missing`; returns whether the variant
    /// actually carries an init segment, so the caller knows to clear the
    /// Layout seed in step.
    pub(super) fn invalidate_init(&self) -> bool {
        if self.init_size() > 0 {
            self.init.state.mark_missing();
            true
        } else {
            false
        }
    }

    pub(super) fn segment_resource(&self, seg_idx: u32) -> Option<ResourceKey> {
        self.segments
            .get(seg_idx as usize)
            .map(|e| e.resource_id.clone())
    }

    /// Media size of segment `idx` — pure media only; the init prefix lives
    /// in its own `[0, init.size)` range.
    pub(super) fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.size.load(Ordering::Acquire))
    }

    /// Index of the first non-`Loaded` segment — the ABR controller's
    /// "download head". Returns `num_segments()` when every segment is
    /// `Loaded`.
    pub(super) fn download_head(&self) -> u32 {
        let head = self
            .segments
            .iter()
            .position(|s| !s.state.is_loaded())
            .unwrap_or(self.segments.len());
        u32::try_from(head).unwrap_or(u32::MAX)
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

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` does
    /// not belong to this variant. Flips `Loaded -> Missing`; queue
    /// reseeding is the caller's job.
    pub(super) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if self.init_size() > 0 && &self.init.resource_id == key {
            self.init.state.mark_missing();
            return Some(-1);
        }
        for (seg_idx, entry) in self.segments.iter().enumerate() {
            if &entry.resource_id == key {
                entry.state.mark_missing();
                return i32::try_from(seg_idx).ok();
            }
        }
        None
    }

    /// Settle-side size store: shrink the appropriate atom to `final_len`.
    /// The caller runs this inside [`Layout::apply_commit`](
    /// super::layout::Layout::apply_commit)'s write-lock so a reader never
    /// observes a new size against a stale offset table.
    pub(super) fn apply_loaded_size(&self, planned: PlannedFetch, final_len: u64) {
        match planned {
            PlannedFetch::Init => self.init.size.store(final_len, Ordering::Release),
            PlannedFetch::Segment(idx) => {
                if let Some(slot) = self.segments.get(idx as usize) {
                    slot.size.store(final_len, Ordering::Release);
                }
            }
        }
    }

    /// Committed on-disk length when `key` is `Committed` with a known
    /// `final_len` — the skip-fetch guard's size source (mirrors what
    /// `FetchSlot::settle` would have applied on a cache-hot resource).
    pub(super) fn committed_final_len(&self, key: &ResourceKey) -> Option<u64> {
        match self.scope.store().resource_state(key) {
            Ok(AssetResourceState::Committed { final_len }) => final_len,
            _ => None,
        }
    }

    pub(super) fn contains_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        self.scope.store().contains_range(key, range)
    }

    /// Open `key` and copy `range` into `dst`. `Ok(None)` means the
    /// resource is not on disk yet (`NotFound`) — the caller treats that as
    /// a pending read. This per-call `open_resource` is the `WS5d` residual
    /// the held-resource lease will retire.
    pub(super) fn read_resource(
        &self,
        key: &ResourceKey,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        let resource = match self.scope.store().open_resource(key, None) {
            Ok(res) => res,
            Err(AssetsError::Io(e)) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StreamError::Source(HlsError::from(e).into())),
        };
        resource
            .wait_range(range.clone())
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        let n = resource
            .read_at(range.start, dst)
            .map_err(|e| StreamError::Source(HlsError::from(e).into()))?;
        Ok(Some(n))
    }
}

fn bisect_right_decode_time(segments: &[SegmentEntry], t: Duration) -> usize {
    let mut lo = 0_usize;
    let mut hi = segments.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if segments[mid].decode_time <= t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
