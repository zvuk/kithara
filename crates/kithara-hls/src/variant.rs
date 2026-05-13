#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::NetError;
use kithara_platform::{Mutex, RwLock, time::Duration};
use kithara_storage::ResourceExt;
use kithara_stream::{
    SegmentDescriptor,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::playlist::{PlaylistAccess, PlaylistState};

pub(crate) mod segment_view;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SegmentState {
    Missing = 0,
    Downloading = 1,
    Loaded = 2,
}

impl From<u8> for SegmentState {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Downloading,
            2 => Self::Loaded,
            _ => Self::Missing,
        }
    }
}

impl From<SegmentState> for Arc<AtomicU8> {
    fn from(state: SegmentState) -> Self {
        Self::new(AtomicU8::new(state as u8))
    }
}

#[derive(Debug)]
struct SegmentEntry {
    url: Url,
    resource_id: ResourceKey,
    byte_offset: u64,
    size: u64,
    /// Shared with the segment's `OnCompleteFn` closure so the
    /// Downloader thread can flip `Downloading -> Loaded` (or back to
    /// `Missing` on error) without re-entering the variant from
    /// outside.
    state: Arc<AtomicU8>,
    decrypt_ctx: Option<DecryptContext>,
    decode_time: Duration,
    duration: Duration,
}

impl SegmentEntry {
    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }
}

#[derive(Debug)]
struct InitEntry {
    url: Url,
    resource_id: ResourceKey,
    size: u64,
    /// Shared with the init segment's `OnCompleteFn`; see
    /// [`SegmentEntry::state`].
    state: Arc<AtomicU8>,
}

impl InitEntry {
    /// Variants without `#EXT-X-MAP` carry this stub. `size == 0` is the
    /// single source of truth for "no init"; `state == Loaded` keeps
    /// `dispatch` from ever emitting a fetch (it only triggers on
    /// `Missing`). The `url`/`resource_id` placeholders are never read
    /// because the size-zero check on every consumer path returns early.
    fn empty() -> Self {
        let url: Url = "about:blank"
            .parse()
            .expect("static placeholder URL parses");
        Self {
            resource_id: ResourceKey::from_url(&url),
            url,
            size: 0,
            state: SegmentState::Loaded.into(),
        }
    }

    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlannedFetch {
    variant: usize,
    segment: u32,
}

pub(crate) struct PlanCtx {
    pub(crate) master_cancel: CancellationToken,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    pub(crate) prefetch_budget: usize,
}

pub(crate) struct HlsVariant {
    variant: usize,
    /// fMP4 init metadata. For raw TS/AAC variants `init.size == 0` and
    /// `init.state == Loaded` — `dispatch` then skips the init step
    /// naturally because no `Missing` transition is possible.
    init: InitEntry,
    segments: Vec<SegmentEntry>,
    queue: Mutex<VecDeque<PlannedFetch>>,
    cancel: RwLock<CancellationToken>,
    position: Arc<AtomicU64>,
}

impl HlsVariant {
    /// Production constructor. Reads parsed playlist metadata and assembles
    /// the per-variant index, init/segment entries, queue, and cancel
    /// hierarchy. `decrypt_contexts[i]` carries the pre-resolved
    /// [`DecryptContext`] for segment `i` (or `None` for cleartext
    /// segments) — the caller resolves AES-128 keys through [`KeyManager`](
    /// crate::loading::KeyManager) before construction.
    #[must_use]
    pub(crate) fn new(
        variant: usize,
        playlist_state: &PlaylistState,
        decrypt_contexts: &[Option<DecryptContext>],
        ctx: &PlanCtx,
    ) -> Self {
        let init = Self::build_init_entry(playlist_state, variant);
        let segments = Self::build_segment_entries(playlist_state, decrypt_contexts, variant);
        Self::from_parts(variant, init, segments, ctx)
    }

    /// Bare assembly: the supplied `init` and `segments` lists are taken
    /// verbatim. Used by unit tests inside this module to construct
    /// variants from hand-built fixtures without parsing a playlist.
    #[must_use]
    fn from_parts(
        variant: usize,
        init: InitEntry,
        segments: Vec<SegmentEntry>,
        ctx: &PlanCtx,
    ) -> Self {
        Self {
            variant,
            init,
            segments,
            queue: Mutex::new(VecDeque::new()),
            cancel: RwLock::new(ctx.master_cancel.child_token()),
            position: Arc::new(AtomicU64::new(0)),
        }
    }

    fn build_init_entry(playlist_state: &PlaylistState, variant_idx: usize) -> InitEntry {
        playlist_state
            .init_url(variant_idx)
            .map_or_else(InitEntry::empty, |url| InitEntry {
                resource_id: ResourceKey::from_url(&url),
                url,
                size: playlist_state.init_size(variant_idx),
                state: SegmentState::Missing.into(),
            })
    }

    fn build_segment_entries(
        playlist_state: &PlaylistState,
        decrypt_contexts: &[Option<DecryptContext>],
        variant_idx: usize,
    ) -> Vec<SegmentEntry> {
        let Some(num) = playlist_state.num_segments(variant_idx) else {
            return Vec::new();
        };
        let mut decode_time = Duration::ZERO;
        let mut out = Vec::with_capacity(num);
        for seg_idx in 0..num {
            let Some(url) = playlist_state.segment_url(variant_idx, seg_idx) else {
                break;
            };
            let byte_offset = playlist_state
                .segment_byte_offset(variant_idx, seg_idx)
                .unwrap_or(0);
            let next_off = playlist_state
                .segment_byte_offset(variant_idx, seg_idx + 1)
                .or_else(|| playlist_state.total_variant_size(variant_idx))
                .unwrap_or(byte_offset);
            let size = next_off.saturating_sub(byte_offset);
            let duration = playlist_state
                .segment_decode_range(variant_idx, seg_idx)
                .map_or(Duration::ZERO, |(start, end)| end.saturating_sub(start));
            let decrypt_ctx = decrypt_contexts.get(seg_idx).cloned().flatten();
            out.push(SegmentEntry {
                resource_id: ResourceKey::from_url(&url),
                url,
                byte_offset,
                size,
                state: SegmentState::Missing.into(),
                decrypt_ctx,
                decode_time,
                duration,
            });
            decode_time = decode_time.saturating_add(duration);
        }
        out
    }

    #[kithara::probe(variant = self.variant as u64, pos = self.position.load(Ordering::Acquire))]
    pub(crate) fn get_position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .binary_search_byte(byte_offset)
            .and_then(|i| u64::try_from(i).ok())
            .unwrap_or(u64::MAX)
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        let idx = self.binary_search_byte(byte_offset)?;
        let entry = &self.segments[idx];
        let idx_u32 = u32::try_from(idx).ok()?;
        Some((idx_u32, entry.byte_offset, entry.size))
    }

    #[kithara::probe(variant = self.variant as u64, total = self.total_bytes_inner())]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes_inner()
    }

    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
    }

    /// Index of the first non-`Loaded` segment — interpreted as the
    /// "download head" by the ABR controller. Returns `num_segments()`
    /// when every segment is `Loaded`. Scans linearly; cheap because it
    /// only runs from `Abr::progress` (ABR tick cadence).
    pub(crate) fn download_head(&self) -> u32 {
        let head = self
            .segments
            .iter()
            .position(|s| !matches!(s.state(), SegmentState::Loaded))
            .unwrap_or(self.segments.len());
        u32::try_from(head).unwrap_or(u32::MAX)
    }

    #[kithara::probe(variant = self.variant as u64, size = self.init.size)]
    pub(crate) fn init_byte_range(&self) -> Option<Range<u64>> {
        (self.init.size > 0).then_some(0..self.init.size)
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC).
    pub(crate) fn init_resource(&self) -> Option<ResourceKey> {
        (self.init.size > 0).then(|| self.init.resource_id.clone())
    }

    /// Cached init segment size; the first [`Self::init_size`] bytes of
    /// segment 0 in variant-byte space resolve to the init resource
    /// rather than the media resource. Zero when no init exists.
    pub(crate) fn init_size(&self) -> u64 {
        self.init.size
    }

    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn descriptor_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        self.descriptor(idx)
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_after_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let mut idx = bisect_left_byte_offset(&self.segments, byte);
        if idx >= self.segments.len() {
            return None;
        }
        if self.segments[idx].byte_offset < byte {
            idx += 1;
        }
        if idx >= self.segments.len() {
            return None;
        }
        self.descriptor(idx)
    }

    /// Reposition the cursor to the segment that covers `target` and
    /// rebuild the queue from there. Returns the resolved segment index,
    /// or `None` when the variant carries no segments.
    pub(crate) fn seek_to(&self, ctx: &PlanCtx, target: Duration) -> Option<u32> {
        let seg = self.segment_index_at_time(target)?;
        let byte = self.segment_byte_offset(seg)?;
        self.set_position(byte);
        self.rebuild(ctx, seg);
        Some(seg)
    }

    /// Take ownership of the reader after an ABR variant flip: place
    /// the cursor at the start of `from_seg` (or `total_bytes()` when
    /// `from_seg` overshoots this variant) and rebuild the queue.
    pub(crate) fn activate_at_segment(&self, ctx: &PlanCtx, from_seg: u32) {
        let byte = if from_seg < self.num_segments() {
            self.segment_byte_offset(from_seg).unwrap_or(0)
        } else {
            self.total_bytes()
        };
        self.set_position(byte);
        self.rebuild(ctx, from_seg);
    }

    /// Reissue the variant's cancel token and refill the queue starting at `from_seg`.
    /// Old token is cancelled — any `FetchCmd` holding a clone observes cancellation.
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn rebuild(&self, ctx: &PlanCtx, from_seg: u32) {
        let new_token = ctx.master_cancel.child_token();
        let old_token = {
            let mut guard = self.cancel.lock_sync_write();
            std::mem::replace(&mut *guard, new_token)
        };
        old_token.cancel();
        self.queue.lock_sync().clear();
        self.fill_queue(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, seg_at_reader)]
    pub(crate) fn on_reader_advance(&self, ctx: &PlanCtx, seg_at_reader: u32) {
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let mut queue = self.queue.lock_sync();
        if matches!(self.state_of(seg_at_reader), SegmentState::Missing)
            && !queue_contains_seg(&queue, seg_at_reader)
        {
            queue.push_front(PlannedFetch {
                variant: self.variant,
                segment: seg_at_reader,
            });
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = seg_at_reader.saturating_add(budget).min(segs_len_u32 - 1);
        let last_planned = queue.iter().next_back().map(|p| p.segment);
        let start = last_planned.map_or(seg_at_reader, |s| s.saturating_add(1));
        for seg in start..=end {
            if matches!(self.state_of(seg), SegmentState::Missing)
                && !queue_contains_seg(&queue, seg)
            {
                queue.push_back(PlannedFetch {
                    variant: self.variant,
                    segment: seg,
                });
            }
        }
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` doesn't belong to this variant.
    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if self.init.size > 0 && &self.init.resource_id == key {
            self.init.set_state(SegmentState::Missing);
            return Some(-1);
        }
        for (seg_idx, entry) in self.segments.iter().enumerate() {
            if &entry.resource_id == key {
                entry.set_state(SegmentState::Missing);
                return i32::try_from(seg_idx).ok();
            }
        }
        None
    }

    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn dispatch(&self, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        if matches!(self.init.state(), SegmentState::Missing) && remaining > 0 {
            self.init.set_state(SegmentState::Downloading);
            out.push(self.build_init_cmd(ctx));
            remaining -= 1;
        }
        while remaining > 0 {
            let Some(planned) = self.queue.lock_sync().pop_front() else {
                break;
            };
            let seg_idx = planned.segment;
            let Some(entry) = self.segments.get(seg_idx as usize) else {
                continue;
            };
            if !matches!(entry.state(), SegmentState::Missing) {
                continue;
            }
            entry.set_state(SegmentState::Downloading);
            out.push(self.build_seg_cmd(ctx, seg_idx));
            remaining -= 1;
        }
        out
    }

    pub(crate) fn cancel_handle(&self) -> CancellationToken {
        self.cancel.lock_sync_read().clone()
    }

    pub(crate) fn cancel(&self) {
        self.cancel.lock_sync_read().cancel();
    }

    pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64> {
        self.segments.get(seg_idx as usize).map(|e| e.byte_offset)
    }

    pub(crate) fn segment_resource(&self, seg_idx: u32) -> Option<ResourceKey> {
        self.segments
            .get(seg_idx as usize)
            .map(|e| e.resource_id.clone())
    }

    pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        u32::try_from(idx).ok()
    }

    fn binary_search_byte(&self, byte_offset: u64) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        let mut lo = 0_usize;
        let mut hi = self.segments.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = &self.segments[mid];
            if byte_offset < entry.byte_offset {
                hi = mid;
            } else if byte_offset >= entry.byte_offset + entry.size {
                lo = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    fn total_bytes_inner(&self) -> u64 {
        self.segments
            .last()
            .map_or(self.init.size, |last| last.byte_offset + last.size)
    }

    fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
        let entry = self.segments.get(idx)?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        Some(SegmentDescriptor::new(
            entry.byte_offset..entry.byte_offset + entry.size,
            entry.decode_time,
            entry.duration,
            seg_idx_u32,
            self.variant,
        ))
    }

    fn fill_queue(&self, ctx: &PlanCtx, from_seg: u32) {
        let segs_len_u32 = self.num_segments();
        if segs_len_u32 == 0 {
            return;
        }
        let budget = u32::try_from(ctx.prefetch_budget).unwrap_or(u32::MAX);
        let end = from_seg.saturating_add(budget).min(segs_len_u32 - 1);
        let mut queue = self.queue.lock_sync();
        for seg in from_seg..=end {
            if matches!(self.state_of(seg), SegmentState::Missing) {
                queue.push_back(PlannedFetch {
                    variant: self.variant,
                    segment: seg,
                });
            }
        }
    }

    fn state_of(&self, seg_idx: u32) -> SegmentState {
        self.segments
            .get(seg_idx as usize)
            .map_or(SegmentState::Missing, SegmentEntry::state)
    }

    fn build_init_cmd(&self, ctx: &PlanCtx) -> FetchCmd {
        let resource = ctx
            .asset_store
            .acquire_resource(&self.init.resource_id)
            .expect("acquire_resource for init must succeed");
        let slot = FetchSlot {
            resource: resource.clone(),
            state: Arc::clone(&self.init.state),
        };
        FetchCmd::get(self.init.url.clone())
            .cancel(Some(self.cancel_handle()))
            .writer(slot.writer())
            .on_complete(slot.into_on_complete())
    }

    fn build_seg_cmd(&self, ctx: &PlanCtx, seg_idx: u32) -> FetchCmd {
        let entry = &self.segments[seg_idx as usize];
        let resource = entry.decrypt_ctx.clone().map_or_else(
            || {
                ctx.asset_store
                    .acquire_resource(&entry.resource_id)
                    .expect("acquire_resource for segment must succeed")
            },
            |ctx_inner| {
                ctx.asset_store
                    .acquire_resource_with_ctx(&entry.resource_id, Some(ctx_inner))
                    .expect("acquire_resource_with_ctx for segment must succeed")
            },
        );
        let slot = FetchSlot {
            resource,
            state: Arc::clone(&entry.state),
        };
        FetchCmd::get(entry.url.clone())
            .cancel(Some(self.cancel_handle()))
            .writer(slot.writer())
            .on_complete(slot.into_on_complete())
    }
}

/// Pairs a freshly-acquired [`AssetResource`] with the shared state flag
/// observed by the scheduler. Owns the closures emitted on a [`FetchCmd`] so
/// settlement (commit-or-fail + state transition) lives in one place.
struct FetchSlot {
    resource: AssetResource<DecryptContext>,
    state: Arc<AtomicU8>,
}

impl FetchSlot {
    fn writer(&self) -> WriterFn {
        let resource = self.resource.clone();
        let offset = Arc::new(AtomicU64::new(0));
        Box::new(move |chunk: &[u8]| {
            let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            resource.write_at(pos, chunk).map_err(IoError::other)
        })
    }

    fn into_on_complete(self) -> OnCompleteFn {
        Box::new(move |_bytes_written, err| self.settle(err))
    }

    fn settle(&self, err: Option<&NetError>) {
        let next = err.map_or_else(
            || {
                self.resource
                    .commit(None)
                    .map_or(SegmentState::Missing, |()| SegmentState::Loaded)
            },
            |e| {
                self.resource.fail(e.to_string());
                SegmentState::Missing
            },
        );
        self.state.store(next as u8, Ordering::Release);
    }
}

fn queue_contains_seg(queue: &VecDeque<PlannedFetch>, seg_idx: u32) -> bool {
    queue.iter().any(|p| p.segment == seg_idx)
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

fn bisect_left_byte_offset(segments: &[SegmentEntry], byte: u64) -> usize {
    let mut lo = 0_usize;
    let mut hi = segments.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if segments[mid].byte_offset < byte {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
