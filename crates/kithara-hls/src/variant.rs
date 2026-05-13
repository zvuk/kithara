#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetResource, AssetResourceState, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::NetError;
use kithara_platform::{Mutex, RwLock, time::Duration};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    SegmentDescriptor,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::playlist::{PlaylistAccess, PlaylistState};

#[cfg(test)]
mod tests;

/// Three-valued cache state. `Downloading` exists to dedupe in-flight
/// fetches: `dispatch` only emits a `FetchCmd` for entries in `Missing`,
/// flipping them to `Downloading` before the cmd leaves. The settle path
/// drives `Downloading -> Loaded` (success or "another writer already
/// committed") and `Downloading -> Missing` (recoverable failure / cancel).
/// Eviction is the only producer of `Loaded -> Missing`.
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
    /// Cache state. Shared with the segment's `FetchSlot`: settle on
    /// success stores `Loaded`; evict stores `Missing`. Stale settles
    /// (cancelled before completion) are gated by `FetchSlot.cancel` and
    /// never reach the store.
    state: Arc<AtomicU8>,
    /// Encrypted media size from HEAD, shrunk to the actual post-decrypt
    /// length by [`HlsVariant::apply_commit`] when `commit` reports
    /// the resource's `final_len`. Reader queries
    /// (`segment_size`, `total_bytes`) read through this atomic.
    size: AtomicU64,
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

    /// Atomic Missing -> Downloading claim. Returns `true` when the
    /// caller now owns the in-flight slot. `dispatch` uses this to
    /// dedupe concurrent fetch emissions for the same segment.
    fn try_claim_for_download(&self) -> bool {
        self.state
            .compare_exchange(
                SegmentState::Missing as u8,
                SegmentState::Downloading as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[derive(Debug)]
struct InitEntry {
    url: Url,
    resource_id: ResourceKey,
    /// Shared with the init segment's `OnCompleteFn`; see
    /// [`SegmentEntry::state`].
    state: Arc<AtomicU8>,
    /// Encrypted init size from HEAD, shrunk on commit — see
    /// [`SegmentEntry::size`].
    size: AtomicU64,
    /// Decryption context for encrypted init segments. HLS init segments
    /// don't carry their own `#EXT-X-KEY`; we mirror the first media
    /// segment's key — the standard packaging convention.
    decrypt_ctx: Option<DecryptContext>,
}

impl InitEntry {
    /// Variants without `#EXT-X-MAP` carry this stub. Pairs with
    /// `size == 0`, so [`HlsVariant::rebuild`] never enqueues `Init`
    /// (we only enqueue when `size > 0`). The `url`/`resource_id`
    /// placeholders are never read.
    fn empty() -> Self {
        let url: Url = "about:blank"
            .parse()
            .expect("static placeholder URL parses");
        Self {
            resource_id: ResourceKey::from_url(&url),
            url,
            state: SegmentState::Loaded.into(),
            size: AtomicU64::new(0),
            decrypt_ctx: None,
        }
    }

    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }

    fn try_claim_for_download(&self) -> bool {
        self.state
            .compare_exchange(
                SegmentState::Missing as u8,
                SegmentState::Downloading as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// One unit of pending fetch work for the variant. `Init` is the only
/// non-segment entry — placed at the front of the queue by `rebuild` so
/// the fMP4 init prefix is fetched before any media segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedFetch {
    Init,
    Segment(u32),
}

pub(crate) struct PlanCtx {
    pub(crate) master_cancel: CancellationToken,
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    pub(crate) prefetch_budget: usize,
}

pub(crate) struct HlsVariant {
    variant: usize,
    /// fMP4 init metadata. For raw TS/AAC variants `init.size == 0` and
    /// `init.state == Loaded` — `rebuild` then never enqueues `Init`.
    init: InitEntry,
    segments: Vec<SegmentEntry>,
    /// Cumulative byte offsets for **media** segments only, seeded with
    /// `init.size` (the init prefix occupies `[0, init.size)` in
    /// variant-byte space). Recomputed on every commit — AES-128 CBC
    /// strips up to 16 bytes per segment on PKCS7 unpad, so HEAD-derived
    /// sizes are upper bounds; without recompute the reader spins on
    /// padding bytes that don't exist on disk.
    offsets: RwLock<Vec<u64>>,
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
        init_decrypt_ctx: Option<DecryptContext>,
        decrypt_contexts: &[Option<DecryptContext>],
        ctx: &PlanCtx,
    ) -> Arc<Self> {
        let init = Self::build_init_entry(playlist_state, variant, init_decrypt_ctx);
        let segments = Self::build_segment_entries(
            playlist_state,
            decrypt_contexts,
            variant,
            init.size.load(Ordering::Acquire),
        );
        Self::from_parts(variant, init, segments, ctx)
    }

    /// Bare assembly used by unit tests inside this module.
    #[must_use]
    fn from_parts(
        variant: usize,
        init: InitEntry,
        segments: Vec<SegmentEntry>,
        ctx: &PlanCtx,
    ) -> Arc<Self> {
        let variant_ref = Self {
            variant,
            init,
            segments,
            offsets: RwLock::new(Vec::new()),
            queue: Mutex::new(VecDeque::new()),
            cancel: RwLock::new(ctx.master_cancel.child_token()),
            position: Arc::new(AtomicU64::new(0)),
        };
        variant_ref.recompute_offsets();
        Arc::new(variant_ref)
    }

    fn build_init_entry(
        playlist_state: &PlaylistState,
        variant_idx: usize,
        decrypt_ctx: Option<DecryptContext>,
    ) -> InitEntry {
        playlist_state
            .init_url(variant_idx)
            .map_or_else(InitEntry::empty, |url| InitEntry {
                resource_id: ResourceKey::from_url(&url),
                url,
                state: SegmentState::Missing.into(),
                size: AtomicU64::new(playlist_state.init_size(variant_idx)),
                decrypt_ctx,
            })
    }

    /// Builds per-segment metadata. Segment 0's `segment_byte_offset` from
    /// the playlist absorbs the init prefix (`size_map` convention) — we
    /// subtract `init_size` so each [`SegmentEntry::size`] holds the
    /// encrypted **media-only** length; subsequent segments are pure
    /// media. `init_size` is the in-memory `AtomicU64` value used as the
    /// running seed for cumulative offsets.
    fn build_segment_entries(
        playlist_state: &PlaylistState,
        decrypt_contexts: &[Option<DecryptContext>],
        variant_idx: usize,
        init_size: u64,
    ) -> Vec<SegmentEntry> {
        let Some(num) = playlist_state.num_segments(variant_idx) else {
            return Vec::new();
        };
        let mut decode_time = Duration::ZERO;
        let mut entries = Vec::with_capacity(num);
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
            let full_size = next_off.saturating_sub(byte_offset);
            let media_size = if seg_idx == 0 {
                full_size.saturating_sub(init_size)
            } else {
                full_size
            };
            let duration = playlist_state
                .segment_decode_range(variant_idx, seg_idx)
                .map_or(Duration::ZERO, |(start, end)| end.saturating_sub(start));
            let decrypt_ctx = decrypt_contexts.get(seg_idx).cloned().flatten();
            entries.push(SegmentEntry {
                resource_id: ResourceKey::from_url(&url),
                url,
                state: SegmentState::Missing.into(),
                size: AtomicU64::new(media_size),
                decrypt_ctx,
                decode_time,
                duration,
            });
            decode_time = decode_time.saturating_add(duration);
        }
        entries
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

    pub(crate) fn init_size(&self) -> u64 {
        self.init.size.load(Ordering::Acquire)
    }

    /// Media size of segment `idx` — pure media only; the init prefix
    /// (when present) lives in its own range `[0, init.size)` and is
    /// served by the source via [`Self::init_resource`].
    fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.size.load(Ordering::Acquire))
    }

    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .find_at_offset_inner(byte_offset)
            .map_or(u64::MAX, |(i, _, _)| u64::from(i))
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.find_at_offset_inner(byte_offset)
    }

    /// Snapshot `(byte_offset, size)` pairs under the read lock so the
    /// caller can binary-search without holding the guard. Reading sizes
    /// inside the guard scope blocks any racing `apply_commit` write —
    /// keeps the snapshot consistent.
    fn layout_snapshot(&self) -> Vec<(u64, u64)> {
        let offsets = self.offsets.lock_sync_read();
        offsets
            .iter()
            .zip(self.segments.iter())
            .map(|(off, seg)| (*off, seg.size.load(Ordering::Acquire)))
            .collect()
    }

    fn find_at_offset_inner(&self, byte: u64) -> Option<(u32, u64, u64)> {
        let snapshot = self.layout_snapshot();
        let mut lo = 0_usize;
        let mut hi = snapshot.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (off, size) = snapshot[mid];
            if byte < off {
                hi = mid;
            } else if byte >= off + size {
                lo = mid + 1;
            } else {
                let idx_u32 = u32::try_from(mid).ok()?;
                return Some((idx_u32, off, size));
            }
        }
        None
    }

    #[kithara::probe(variant = self.variant as u64, total = self.total_bytes_inner())]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes_inner()
    }

    fn total_bytes_inner(&self) -> u64 {
        let offsets = self.offsets.lock_sync_read();
        match (offsets.last(), self.segments.len().checked_sub(1)) {
            (Some(&off), Some(idx)) => off + self.segment_size(idx).unwrap_or(0),
            // No segments — total is just the init prefix (zero for raw
            // TS/AAC variants, `init.size` for fMP4).
            _ => self.init_size(),
        }
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

    #[kithara::probe(variant = self.variant as u64, size = self.init_size())]
    pub(crate) fn init_byte_range(&self) -> Option<Range<u64>> {
        let size = self.init_size();
        (size > 0).then_some(0..size)
    }

    /// Byte range a demuxer should read to re-establish container state
    /// after a format change (HLS variant flip or codec change).
    ///
    /// fMP4 variants advertise an `#EXT-X-MAP` init segment — its range
    /// is `0..init_size`. Containers that embed the file header inside
    /// the first media segment (raw WAV / PCM with leading header) fall
    /// back to segment 0's byte range so the demuxer can re-parse the
    /// header from there. Containers with implicit framing (AAC ADTS,
    /// MP3, MPEG-TS) do not need this range — the caller filters via
    /// `container_needs_init_range` before reading.
    pub(crate) fn header_byte_range(&self) -> Option<Range<u64>> {
        if let Some(range) = self.init_byte_range() {
            return Some(range);
        }
        let (_, off, size) = self.find_at_offset_inner(0)?;
        Some(off..(off + size))
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC).
    pub(crate) fn init_resource(&self) -> Option<ResourceKey> {
        (self.init_size() > 0).then(|| self.init.resource_id.clone())
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
        let mut idx = self.bisect_left_byte_offset(byte);
        if idx >= self.segments.len() {
            return None;
        }
        let off = self.segment_byte_offset_usize(idx)?;
        if off < byte {
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

    /// Replace the per-variant fetch queue with `[from_seg .. num_segments)`
    /// (plus `Init` if applicable). Does NOT cancel in-flight fetches —
    /// dedup is handled at `dispatch` time via the `Downloading` state.
    /// `dispatch` skips `Downloading` and `Loaded` entries without burning
    /// budget, so the queue can safely include them.
    ///
    /// Cancellation is reserved for variant deactivation
    /// ([`cancel`](Self::cancel) / teardown) — there we really want to
    /// abandon the variant's in-flight work; the freshly activated variant
    /// has its own cancel token. Seek / eviction never need to cancel,
    /// they only need to reseed the queue.
    ///
    /// Callers: seek (`seek_to`), ABR variant flip
    /// (`activate_at_segment`), eviction of an active-variant resource,
    /// and the initial peer activation.
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn rebuild(&self, _ctx: &PlanCtx, from_seg: u32) {
        let segs_len = self.num_segments();
        let mut queue = self.queue.lock_sync();
        queue.clear();
        if self.init_size() > 0 && !matches!(self.init.state(), SegmentState::Loaded) {
            queue.push_back(PlannedFetch::Init);
        }
        for seg in from_seg..segs_len {
            queue.push_back(PlannedFetch::Segment(seg));
        }
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` doesn't belong to this variant.
    /// State flips `Loaded -> Missing`; queue reseeding is the caller's job
    /// (see `HlsCoord::broadcast_eviction` → `rebuild` for the active
    /// variant; non-active variants' queues are rebuilt lazily on the
    /// next ABR flip).
    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        if self.init_size() > 0 && &self.init.resource_id == key {
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
    pub(crate) fn dispatch(self: &Arc<Self>, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        while remaining > 0 {
            let Some(planned) = self.queue.lock_sync().pop_front() else {
                break;
            };
            match planned {
                PlannedFetch::Init => {
                    if !self.init.try_claim_for_download() {
                        continue;
                    }
                    if Self::resource_already_committed(ctx, &self.init.resource_id) {
                        self.init.set_state(SegmentState::Loaded);
                        continue;
                    }
                    debug!(target: "kithara_hls::dispatch", v = self.variant, "emit init");
                    out.push(self.build_init_cmd(ctx));
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.segments.get(seg_idx as usize) else {
                        continue;
                    };
                    if !entry.try_claim_for_download() {
                        continue;
                    }
                    if Self::resource_already_committed(ctx, &entry.resource_id) {
                        entry.set_state(SegmentState::Loaded);
                        continue;
                    }
                    debug!(target: "kithara_hls::dispatch", v = self.variant, seg = seg_idx, "emit seg");
                    out.push(self.build_seg_cmd(ctx, seg_idx));
                }
            }
            remaining -= 1;
        }
        out
    }

    /// Skip-fetch guard: a previously-cached resource may stay
    /// `Committed` on disk even after the in-memory LRU evicts it
    /// (eviction clears the cache slot, not the on-disk bytes). The reader's
    /// `contains_range` already falls back to `resource_state` so the
    /// segment is readable; dispatching a fresh `acquire_resource` against
    /// a committed key would race the existing writer and fail with
    /// `cannot write to committed resource`.
    fn resource_already_committed(ctx: &PlanCtx, key: &ResourceKey) -> bool {
        matches!(
            ctx.asset_store.resource_state(key),
            Ok(AssetResourceState::Committed { .. })
        )
    }

    pub(crate) fn cancel_handle(&self) -> CancellationToken {
        self.cancel.lock_sync_read().clone()
    }

    pub(crate) fn cancel(&self) {
        self.cancel.lock_sync_read().cancel();
    }

    pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64> {
        self.segment_byte_offset_usize(seg_idx as usize)
    }

    fn segment_byte_offset_usize(&self, idx: usize) -> Option<u64> {
        self.offsets.lock_sync_read().get(idx).copied()
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

    /// Recompute cumulative media offsets seeded with `init.size`.
    /// Holds the write-lock briefly; readers see a consistent snapshot
    /// afterwards.
    fn recompute_offsets(&self) {
        let mut offsets = self.offsets.lock_sync_write();
        self.recompute_offsets_locked(&mut offsets);
    }

    fn recompute_offsets_locked(&self, offsets: &mut Vec<u64>) {
        offsets.resize(self.segments.len(), 0);
        let mut cum = self.init_size();
        for (i, s) in self.segments.iter().enumerate() {
            offsets[i] = cum;
            cum += s.size.load(Ordering::Acquire);
        }
    }

    /// Settle hook: shrinks the appropriate size atom to `actual` and
    /// rebuilds the offset map. Called from
    /// [`FetchSlot::settle`] via `Weak<HlsVariant>::upgrade()` once the
    /// resource commits — for DRM, this is where the post-PKCS7 length
    /// replaces the encrypted estimate.
    ///
    /// Size store and offset recompute happen under the same write
    /// lock — a reader that races in between would see a new size with
    /// stale offsets and fall into a non-existent gap, hanging on
    /// `range_ready`.
    fn apply_commit(&self, planned: PlannedFetch, actual: u64) {
        let mut offsets = self.offsets.lock_sync_write();
        match planned {
            PlannedFetch::Init => self.init.size.store(actual, Ordering::Release),
            PlannedFetch::Segment(idx) => {
                if let Some(slot) = self.segments.get(idx as usize) {
                    slot.size.store(actual, Ordering::Release);
                }
            }
        }
        self.recompute_offsets_locked(&mut offsets);
    }

    fn bisect_left_byte_offset(&self, byte: u64) -> usize {
        let offsets = self.offsets.lock_sync_read();
        let mut lo = 0_usize;
        let mut hi = offsets.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if offsets[mid] < byte {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
        let entry = self.segments.get(idx)?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        let byte_offset = self.segment_byte_offset_usize(idx)?;
        let size = self.segment_size(idx)?;
        Some(SegmentDescriptor::new(
            byte_offset..byte_offset + size,
            entry.decode_time,
            entry.duration,
            seg_idx_u32,
            self.variant,
        ))
    }

    fn build_init_cmd(self: &Arc<Self>, ctx: &PlanCtx) -> FetchCmd {
        let resource = self.init.decrypt_ctx.clone().map_or_else(
            || {
                ctx.asset_store
                    .acquire_resource(&self.init.resource_id)
                    .expect("acquire_resource for init must succeed")
            },
            |ctx_inner| {
                ctx.asset_store
                    .acquire_resource_with_ctx(&self.init.resource_id, Some(ctx_inner))
                    .expect("acquire_resource_with_ctx for init must succeed")
            },
        );
        self.build_cmd(
            self.init.url.clone(),
            resource,
            Arc::clone(&self.init.state),
            PlannedFetch::Init,
        )
    }

    fn build_seg_cmd(self: &Arc<Self>, ctx: &PlanCtx, seg_idx: u32) -> FetchCmd {
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
        self.build_cmd(
            entry.url.clone(),
            resource,
            Arc::clone(&entry.state),
            PlannedFetch::Segment(seg_idx),
        )
    }

    /// Common assembly for init and segment fetches. Both go through the
    /// same `FetchSlot`: writer streams to the asset resource, `on_complete`
    /// runs `settle` which observes `cancel.is_cancelled()` as the epoch
    /// gate.
    fn build_cmd(
        self: &Arc<Self>,
        url: Url,
        resource: AssetResource<DecryptContext>,
        state: Arc<AtomicU8>,
        planned: PlannedFetch,
    ) -> FetchCmd {
        let cancel = self.cancel_handle();
        let slot = FetchSlot {
            resource,
            state,
            cancel: cancel.clone(),
            variant: Arc::downgrade(self),
            planned,
        };
        FetchCmd::get(url)
            .cancel(Some(cancel))
            .writer(slot.writer())
            .on_complete(slot.into_on_complete())
    }
}

/// Pairs the freshly-acquired [`AssetResource`] with the entry's state
/// atom and the cancel token captured at dispatch time. `settle` reads
/// `cancel.is_cancelled()` as the rebuild-epoch marker: a stale fetch
/// (cancelled before completion) does not write to state — `rebuild`
/// has already taken over and the asset slot belongs to the new epoch.
///
/// The `Weak<HlsVariant>` lets the slot call back into the variant to
/// apply the post-decrypt size — we use `Weak` (not `Arc`) so a dropped
/// peer doesn't keep the variant alive past teardown.
struct FetchSlot {
    resource: AssetResource<DecryptContext>,
    state: Arc<AtomicU8>,
    cancel: CancellationToken,
    variant: Weak<HlsVariant>,
    planned: PlannedFetch,
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
        Box::new(move |bytes_written, err| self.settle(bytes_written, err))
    }

    /// On success, commits the resource. `bytes_written` is forwarded as
    /// `final_len` — required by [`ProcessedResource::commit`] to trigger
    /// the post-write decrypt pass on encrypted segments (passing `None`
    /// silently skips decryption, leaving ciphertext on disk). After a
    /// successful commit we read back the resource's `final_len` and
    /// shrink the variant's layout to match: for DRM segments PKCS7
    /// strips up to 16 bytes off the encrypted size, so HEAD-based
    /// estimates are always upper bounds.
    fn settle(&self, bytes_written: u64, err: Option<&NetError>) {
        // Variant-cancelled: the variant has been deactivated (ABR flip
        // away or teardown). Don't touch state — the variant is no longer
        // read; the new active variant has its own state machine.
        if self.cancel.is_cancelled() {
            debug!(target: "kithara_hls::settle", "stale (cancelled)");
            return;
        }
        match err {
            None => {
                let commit_ok = self.resource.commit(Some(bytes_written)).is_ok();
                debug!(target: "kithara_hls::settle", commit_ok, bytes_written, "success");
                let next = if commit_ok {
                    let actual = match self.resource.status() {
                        ResourceStatus::Committed { final_len: Some(n) } => n,
                        _ => bytes_written,
                    };
                    if let Some(v) = self.variant.upgrade() {
                        v.apply_commit(self.planned, actual);
                    }
                    SegmentState::Loaded
                } else {
                    SegmentState::Missing
                };
                self.state.store(next as u8, Ordering::Release);
            }
            Some(e) => {
                // If the resource is already `Committed` we lost a race
                // to another writer that produced the bytes — accept the
                // outcome as `Loaded` instead of poisoning the resource
                // via `fail`. Otherwise reset to `Missing` so a future
                // `rebuild` can retry (eviction broadcast / explicit
                // re-enqueue).
                let committed = matches!(self.resource.status(), ResourceStatus::Committed { .. });
                debug!(target: "kithara_hls::settle", err = %e, committed, "fail-path");
                let next = if committed {
                    if let ResourceStatus::Committed { final_len: Some(n) } = self.resource.status()
                        && let Some(v) = self.variant.upgrade()
                    {
                        v.apply_commit(self.planned, n);
                    }
                    SegmentState::Loaded
                } else {
                    self.resource.fail(e.to_string());
                    SegmentState::Missing
                };
                self.state.store(next as u8, Ordering::Release);
            }
        }
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
