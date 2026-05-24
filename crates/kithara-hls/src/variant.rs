#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::{Error as IoError, ErrorKind},
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

use kithara_assets::{AssetResource, AssetResourceState, AssetStore, AssetsError, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::{Headers, NetError};
use kithara_platform::{
    Mutex, RwLock,
    thread::sleep,
    time::{Duration, Instant},
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, ResourceStatus, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, PendingReason, ReadOutcome, SegmentDescriptor,
    SourceError, SourcePhase, SourceSeekAnchor, StreamError, StreamResult, Timeline,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;

use crate::{
    HlsError,
    playlist::{PlaylistAccess, PlaylistState},
};

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
pub(crate) struct SegmentEntry {
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
    decode_time: Duration,
    duration: Duration,
    decrypt_ctx: Option<DecryptContext>,
    resource_id: ResourceKey,
    url: Url,
}

impl SegmentEntry {
    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }

    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
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
pub(crate) struct InitEntry {
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
    resource_id: ResourceKey,
    url: Url,
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
            resource_id: ResourceKey::from(&url),
            url,
            state: SegmentState::Loaded.into(),
            size: AtomicU64::new(0),
            decrypt_ctx: None,
        }
    }

    fn set_state(&self, s: SegmentState) {
        self.state.store(s as u8, Ordering::Release);
    }

    fn state(&self) -> SegmentState {
        SegmentState::from(self.state.load(Ordering::Acquire))
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
    pub(crate) asset_store: Arc<AssetStore<DecryptContext>>,
    pub(crate) master_cancel: CancellationToken,
    /// Per-resource HTTP headers applied to every init/segment fetch.
    /// Mirrors `HlsConfig::headers`; threaded through so DRM-style auth
    /// tokens carried by the playlist load also reach segment GETs.
    pub(crate) headers: Option<Headers>,
    /// Max bytes the downloader may be ahead of the reader before
    /// `dispatch` pauses emitting `FetchCmd`s. Mirrors
    /// `HlsConfig::look_ahead_bytes`:
    /// - `Some(n)` — when a segment's start offset exceeds
    ///   `variant.position() + n`, leave it (and everything after)
    ///   in the queue; further prefetch waits for the reader to
    ///   advance.
    /// - `None` — no cap (download as fast as possible).
    ///
    /// `Init` is always emitted regardless — the fMP4 demuxer needs
    /// it before any segment can decode.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Snapshot of `Timeline::seek_epoch()` at plan-time. Tagged on
    /// every emitted `FetchCmd`'s probe so integration tests can
    /// distinguish fetches that pre-date a user seek from those that
    /// the scheduler issued *after* observing the new epoch.
    pub(crate) seek_epoch: u64,
    pub(crate) prefetch_budget: usize,
}

pub(crate) struct HlsVariant {
    /// Per-track asset store — used by reader paths (`read_at`,
    /// `wait_range`, `range_ready`) for `open_resource` /
    /// `contains_range` and by `dispatch` (already accessed via
    /// `PlanCtx`) for `acquire_resource`. Cloned from `PlanCtx` at
    /// construction so reader-side methods don't have to thread `ctx`
    /// through.
    asset_store: Arc<AssetStore<DecryptContext>>,
    /// Parsed master/media-playlist data. Owned by `Arc` so multiple
    /// variants share a single immutable view; used by
    /// `seek_time_anchor` (`find_seek_point_for_time`) and as the
    /// source for the cached [`Self::codec`] / [`Self::container`].
    playlist_state: Arc<PlaylistState>,
    position: Arc<AtomicU64>,
    /// Virtual = natural + `byte_shift`. Initial activate keeps shift = 0
    /// so reader sees init at `[0, init_size)` and segments naturally.
    /// Auto-mode variant switch pins shift = `seg_boundary` - natural
    /// offset of `from_seg`, so `v_new`'s `from_seg` lands exactly on the
    /// outgoing variant's segment boundary — the combined byte stream
    /// stays contiguous across switches and fMP4 box addresses remain
    /// aligned.
    byte_shift: AtomicI64,
    /// First media segment served by this variant in the combined byte
    /// stream (inclusive). Initial activate: 0. After switch: the
    /// `from_seg` passed to `activate_at_segment`. Segments < this index
    /// are NOT downloaded and their byte ranges fall in previous
    /// variants' territory.
    served_from: AtomicU32,
    /// Last media segment served (exclusive). For active variant equals
    /// `num_segments()`; for historical (previously-active) variants
    /// shrunk to the successor's `served_from` when deactivated, so
    /// historical lookups only see the segments this variant actually
    /// streamed.
    served_until: AtomicU32,
    /// Frozen `init.size` used as the seed in `recompute_offsets` for
    /// **switched** variants (those activated via Auto-mode mid-stream
    /// with `byte_shift != 0`). `byte_shift` is pinned against the
    /// natural offsets observed at activation; if a later init-segment
    /// decrypt shrunk `init.size`, recomputing against the dynamic
    /// `init.size` would slide every segment offset back by the
    /// PKCS7-stripped delta and break the cross-variant byte address
    /// space alignment. `0` means "no override — use current
    /// `init.size`" (the initial-activation path).
    init_seed: AtomicU64,
    prefetch_anchor: AtomicU64,
    /// Track-level cancel parent (mirror of `coord.cancel` =
    /// `PlanCtx::master_cancel`). Survives variant re-activation:
    /// a cross-codec `commit_variant_switch` may flip from `v_old` to
    /// `v_new` and back to `v_old`, and the second activation of `v_old`
    /// must dispatch fetches under a *live* cancel — see
    /// [`Self::rearm_cancel`]. Cancel hierarchy: `master_cancel` is the
    /// per-track parent created by `HlsPeer`; `cancel` (below) is a
    /// child of `master_cancel`, rotated on every re-activation.
    master_cancel: CancellationToken,
    /// fMP4 init metadata. For raw TS/AAC variants `init.size == 0` and
    /// `init.state == Loaded` — `rebuild` then never enqueues `Init`.
    init: InitEntry,
    queue: Mutex<VecDeque<PlannedFetch>>,
    /// Reader→peer wake handle, installed once when the owning `HlsPeer`
    /// binds. `wait_range` / `seek_time_anchor` fire it so
    /// `HlsPeer::poll_next` resumes on the next event loop tick.
    /// Empty until [`Self::set_peer_wake`] is called by `HlsCoord`.
    peer_wake: OnceLock<Arc<Notify>>,
    /// Cached audio codec — pulled from `playlist_state` at construction
    /// time. The reader's hot path (`media_info`) reads this without
    /// taking the playlist's per-variant `RwLock`.
    codec: Option<AudioCodec>,
    /// Cached container format — see [`Self::codec`].
    container: Option<ContainerFormat>,
    /// HTTP headers applied to every `FetchCmd` this variant emits.
    /// Snapshotted from `PlanCtx::headers` at construction; carries
    /// resource-wide auth (e.g. zvuk `X-Auth-Token`) so segment GETs
    /// reach the same authenticated endpoint as the playlist load.
    headers: Option<Headers>,
    cancel: RwLock<CancellationToken>,
    /// Cumulative byte offsets for **media** segments only, seeded with
    /// `init.size` (the init prefix occupies `[0, init.size)` in
    /// variant-byte space). Recomputed on every commit — AES-128 CBC
    /// strips up to 16 bytes per segment on PKCS7 unpad, so HEAD-derived
    /// sizes are upper bounds; without recompute the reader spins on
    /// padding bytes that don't exist on disk.
    offsets: RwLock<Vec<u64>>,
    /// Track timeline — used by reader-path methods (`phase_at`,
    /// `wait_range`) for `is_flushing` checks. Cheap to clone.
    timeline: Timeline,
    segments: Vec<SegmentEntry>,
    variant: usize,
}

/// Media payload + shared-dep snapshot for [`HlsVariant::from_parts`].
/// Pulled out of the constructor's argument list so the call site stays
/// readable — `new()` builds this from parsed playlist metadata, tests
/// build it inline from synthesised fixtures.
pub(crate) struct VariantParts {
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) init: InitEntry,
    pub(crate) codec: Option<AudioCodec>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) timeline: Timeline,
    pub(crate) segments: Vec<SegmentEntry>,
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
        playlist_state: &Arc<PlaylistState>,
        timeline: &Timeline,
        init_decrypt_ctx: Option<DecryptContext>,
        decrypt_contexts: &[Option<DecryptContext>],
        ctx: &PlanCtx,
    ) -> Arc<Self> {
        let init = Self::build_init_entry(playlist_state.as_ref(), variant, init_decrypt_ctx);
        let segments = Self::build_segment_entries(
            playlist_state.as_ref(),
            decrypt_contexts,
            variant,
            init.size.load(Ordering::Acquire),
        );
        let codec = playlist_state.variant_codec(variant);
        let container = playlist_state.variant_container(variant);
        Self::from_parts(
            variant,
            VariantParts {
                codec,
                container,
                init,
                segments,
                playlist_state: Arc::clone(playlist_state),
                timeline: timeline.clone(),
            },
            ctx,
        )
    }

    /// Auto-mode switch activation. Two byte positions matter:
    ///
    /// - `seg_boundary` — the **virtual** byte where this variant's
    ///   segment `from_seg` should start in the combined stream. The
    ///   caller should pass the outgoing variant's segment boundary
    ///   (e.g. `v_old.segment_byte_offset(from_seg)`) so the byte
    ///   address space joins without gaps or overlaps; box scans across
    ///   the boundary stay correctly aligned.
    /// - `reader_pos` — the reader's current source position. Stored as
    ///   the new variant's `position` so `coord.position()` stays
    ///   monotonic after the active variant flips. May be `>= seg_boundary`
    ///   when the reader had already partially read into `from_seg`'s
    ///   byte range from the outgoing variant before the commit fired.
    ///
    /// `byte_shift` is derived from `seg_boundary`, not `reader_pos`, so
    /// segment offsets pin to actual fMP4 box boundaries.
    pub(crate) fn activate_at_segment_with_shift(
        &self,
        ctx: &PlanCtx,
        from_seg: u32,
        seg_boundary: u64,
        reader_pos: u64,
    ) {
        self.rearm_cancel();
        let from_seg = from_seg.min(self.num_segments());
        let init_at_activation = self.init_size().max(1);
        self.init_seed.store(init_at_activation, Ordering::Release);
        self.recompute_offsets();
        let natural_offset = self
            .segment_byte_offset_natural(from_seg)
            .unwrap_or_else(|| self.total_bytes_inner());
        let shift = i64::try_from(seg_boundary)
            .ok()
            .zip(i64::try_from(natural_offset).ok())
            .and_then(|(v, n)| v.checked_sub(n))
            .unwrap_or(0);
        self.byte_shift.store(shift, Ordering::Release);
        self.served_from.store(from_seg, Ordering::Release);
        self.served_until
            .store(self.num_segments(), Ordering::Release);
        self.set_position(reader_pos);
        self.rebuild(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
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
            planned,
            cancel: cancel.clone(),
            variant: Arc::downgrade(self),
        };
        FetchCmd::get(url)
            .cancel(cancel)
            .maybe_headers(self.headers.clone())
            .writer(slot.writer())
            .on_complete(OnCompleteFn::from(slot))
            .build()
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

    fn build_init_entry(
        playlist_state: &PlaylistState,
        variant_idx: usize,
        decrypt_ctx: Option<DecryptContext>,
    ) -> InitEntry {
        playlist_state
            .init_url(variant_idx)
            .map_or_else(InitEntry::empty, |url| InitEntry {
                resource_id: ResourceKey::from(&url),
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
                resource_id: ResourceKey::from(&url),
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

    pub(crate) fn byte_shift(&self) -> i64 {
        self.byte_shift.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.cancel.lock_sync_read().cancel();
    }

    pub(crate) fn cancel_handle(&self) -> CancellationToken {
        self.cancel.lock_sync_read().clone()
    }

    /// Skip-fetch guard: a previously-cached resource may stay
    /// `Committed` on disk even after the in-memory LRU evicts it
    /// (eviction clears the cache slot, not the on-disk bytes). The reader's
    /// `contains_range` already falls back to `resource_state` so the
    /// segment is readable; dispatching a fresh `acquire_resource` against
    /// a committed key would race the existing writer and fail with
    /// `cannot write to committed resource`.
    ///
    /// Returns the committed on-disk length when the resource is
    /// `Committed` with a known `final_len`. The caller uses that length
    /// to mirror what `FetchSlot::settle` would have done — apply the
    /// post-processing size so `init_size` / per-segment sizes match the
    /// actual bytes on disk (after PKCS7 unpad, container framing, etc).
    /// Without that apply, the announced/estimated size and the on-disk
    /// size disagree and `range_ready` deadlocks on a cache-hot resource.
    fn committed_final_len(ctx: &PlanCtx, key: &ResourceKey) -> Option<u64> {
        match ctx.asset_store.resource_state(key) {
            Ok(AssetResourceState::Committed { final_len }) => final_len,
            _ => None,
        }
    }

    pub(crate) fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
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

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_at_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let (idx, _, _) = self.find_at_offset(byte)?;
        self.descriptor(idx as usize)
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

    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.queue.lock_sync().len() as u64
    )]
    #[kithara::hang_watchdog]
    pub(crate) fn dispatch(self: &Arc<Self>, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        let mut remaining = budget;
        let prefetch_base = self.get_position().max(self.prefetch_anchor());
        let prefetch_byte_cap = ctx
            .look_ahead_bytes
            .map(|n| prefetch_base.saturating_add(n));
        while remaining > 0 {
            hang_tick!();
            let planned = {
                let mut queue = self.queue.lock_sync();
                match queue.front().copied() {
                    None => break,
                    Some(PlannedFetch::Init) => queue.pop_front(),
                    Some(PlannedFetch::Segment(seg_idx)) => {
                        if let Some(cap) = prefetch_byte_cap
                            && let Some(seg_off) = self.segment_byte_offset(seg_idx)
                            && seg_off > cap
                        {
                            break;
                        }
                        queue.pop_front()
                    }
                }
            };
            let Some(planned) = planned else { break };
            match planned {
                PlannedFetch::Init => {
                    if !self.init.try_claim_for_download() {
                        continue;
                    }
                    if let Some(actual) = Self::committed_final_len(ctx, &self.init.resource_id) {
                        self.apply_commit(PlannedFetch::Init, actual);
                        self.init.set_state(SegmentState::Loaded);
                        continue;
                    }
                    out.push(self.build_init_cmd(ctx));
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.segments.get(seg_idx as usize) else {
                        continue;
                    };
                    if !entry.try_claim_for_download() {
                        continue;
                    }
                    if let Some(actual) = Self::committed_final_len(ctx, &entry.resource_id) {
                        self.apply_commit(PlannedFetch::Segment(seg_idx), actual);
                        entry.set_state(SegmentState::Loaded);
                        continue;
                    }
                    out.push(self.emit_fetch_cmd(ctx, seg_idx));
                }
            }
            remaining -= 1;
        }
        out
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

    #[kithara::probe(
        seek_epoch = ctx.seek_epoch,
        segment_index = u64::from(seg_idx),
        variant = self.variant as u64
    )]
    fn emit_fetch_cmd(self: &Arc<Self>, ctx: &PlanCtx, seg_idx: u32) -> FetchCmd {
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

    /// Reader-facing lookup in **virtual** byte space. Subtracts the
    /// variant's `byte_shift` so the inner binary search runs against the
    /// natural offset table (`offsets[]`), then re-shifts the segment's
    /// reported `byte_offset` back into virtual space. Returns `None` when
    /// the byte falls outside the variant's served range
    /// `[served_from..served_until)` so cross-variant lookups in
    /// [`HlsCoord::find_at_offset`] fall through to the previous variant.
    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .find_virtual(byte_offset)
            .map_or(u64::MAX, |(i, _, _)| u64::from(i))
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.find_virtual(byte_offset)
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

    fn find_virtual(&self, byte_virtual: u64) -> Option<(u32, u64, u64)> {
        let shift = self.byte_shift();
        let byte_natural = i64::try_from(byte_virtual).ok()?.checked_sub(shift)?;
        if byte_natural < 0 {
            return None;
        }
        let byte_natural = u64::try_from(byte_natural).ok()?;
        let (idx, off_nat, size) = self.find_at_offset_inner(byte_natural)?;
        if idx < self.served_from() || idx >= self.served_until() {
            return None;
        }
        let off_virtual = u64::try_from(i64::try_from(off_nat).ok()?.checked_add(shift)?).ok()?;
        Some((idx, off_virtual, size))
    }

    /// Header byte range for format-change resync — alias for
    /// [`Self::header_byte_range`] under the `Source` trait's name.
    pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.header_byte_range()
    }

    /// Bare assembly used by unit tests inside this module.
    #[must_use]
    fn from_parts(variant: usize, parts: VariantParts, ctx: &PlanCtx) -> Arc<Self> {
        let num = u32::try_from(parts.segments.len()).unwrap_or(u32::MAX);
        let variant_ref = Self {
            variant,
            asset_store: Arc::clone(&ctx.asset_store),
            playlist_state: parts.playlist_state,
            timeline: parts.timeline,
            peer_wake: OnceLock::new(),
            codec: parts.codec,
            container: parts.container,
            init: parts.init,
            segments: parts.segments,
            offsets: RwLock::new(Vec::new()),
            init_seed: AtomicU64::new(0),
            byte_shift: AtomicI64::new(0),
            prefetch_anchor: AtomicU64::new(0),
            served_from: AtomicU32::new(0),
            served_until: AtomicU32::new(num),
            queue: Mutex::new(VecDeque::new()),
            master_cancel: ctx.master_cancel.clone(),
            cancel: RwLock::new(ctx.master_cancel.child_token()),
            headers: ctx.headers.clone(),
            position: Arc::new(AtomicU64::new(0)),
        };
        variant_ref.recompute_offsets();
        Arc::new(variant_ref)
    }

    #[kithara::probe(variant = self.variant as u64, pos = self.position.load(Ordering::Acquire))]
    pub(crate) fn get_position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
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
    ///
    /// Header byte range for cross-codec format-change recovery.
    ///
    /// `Ok(range)` only when recovery is actually applicable:
    /// - `served_from() == 0` AND `init.size > 0`: virtual init range
    ///   `[0..init_size)`. The decoder factory's Symphonia probe re-
    ///   reads init from here.
    /// - `served_from() == 0` AND `init.size == 0` (raw TS/AAC): start
    ///   of segment 0 — the demuxer re-parses the header from there.
    ///
    /// `Err(FormatChangeNotApplicable)` when the variant was
    /// activated by [`Self::activate_at_segment_with_shift`]
    /// (same-codec ABR commit) and has `served_from() > 0`. Init
    /// bytes live at natural `[0..init_size)` while virtual space
    /// starts at `byte_shift` — same-codec playback continues
    /// through the byte-shift without needing init recovery. Format-
    /// change recovery by design does not apply to byte-shifted
    /// variants; cross-codec recovery only runs after
    /// [`Self::reset_to_full_range`] zeroes the shift.
    pub(crate) fn header_byte_range(&self) -> StreamResult<Range<u64>> {
        if self.served_from() != 0 {
            return Err(StreamError::Source(SourceError::FormatChangeNotApplicable));
        }
        if self.init_size() > 0 {
            return Ok(self.init_byte_range());
        }
        let (_, off, size) = self.find_at_offset_inner(0).ok_or_else(|| {
            StreamError::Source(SourceError::Other(Box::new(IoError::other(
                "variant has no segments — cannot derive header range",
            ))))
        })?;
        Ok(off..(off + size))
    }

    /// Init segment range in **natural** byte space — always
    /// `0..init_size`, regardless of post-commit `served_from`. Returns
    /// an empty range (`0..0`) when the variant has no `#EXT-X-MAP`
    /// init (raw TS/AAC/MPEG-ES).
    ///
    /// The "is this init addressable in the merged virtual space?"
    /// question lives in the *caller* (e.g. `init_descriptor_at`) which
    /// combines this with `served_from()` — keeping virtual-space
    /// concerns out of a per-variant primitive avoids silently dropping
    /// post-commit inits at the `SegmentLayout` boundary.
    #[kithara::probe(variant = self.variant as u64, size = self.init_size())]
    pub(crate) fn init_byte_range(&self) -> Range<u64> {
        0..self.init_size()
    }

    /// Init prefix descriptor for the byte at `byte_offset`. Returns
    /// `None` when the byte falls outside this variant's *virtually
    /// addressable* init range — the init only counts when
    /// `served_from() == 0` (post-commit, init is orphaned in natural
    /// space).
    pub(crate) fn init_descriptor_at(&self, byte_offset: u64) -> Option<(ResourceKey, Range<u64>)> {
        if self.served_from() != 0 {
            return None;
        }
        let range = self.init_byte_range();
        if range.is_empty() || !range.contains(&byte_offset) {
            return None;
        }
        Some((self.init_resource()?, range))
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC).
    pub(crate) fn init_resource(&self) -> Option<ResourceKey> {
        (self.init_size() > 0).then(|| self.init.resource_id.clone())
    }

    pub(crate) fn init_size(&self) -> u64 {
        self.init.size.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate_init(&self) {
        if self.init_size() > 0 {
            self.init.set_state(SegmentState::Missing);
            self.init_seed.store(0, Ordering::Release);
        }
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

    /// Media descriptor for the segment covering `byte_offset` —
    /// `(resource_key, segment_byte_offset, segment_size)`.
    pub(crate) fn media_descriptor(&self, byte_offset: u64) -> Option<(ResourceKey, u64, u64)> {
        let (seg_idx, seg_off, seg_size) = self.find_at_offset(byte_offset)?;
        let key = self.segment_resource(seg_idx)?;
        Some((key, seg_off, seg_size))
    }

    pub(crate) fn media_info(&self) -> MediaInfo {
        let variant_u32 = u32::try_from(self.variant).unwrap_or(u32::MAX);
        MediaInfo::builder()
            .maybe_codec(self.codec)
            .maybe_container(self.container)
            .variant_index(variant_u32)
            .build()
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
    /// Whether the next dispatch should issue the separate init fetch
    /// (CMAF `EXT-X-MAP`) — true only if the variant advertises a
    /// non-zero init segment that hasn't been loaded yet.
    fn needs_init_fetch(&self) -> bool {
        self.init_size() > 0 && !matches!(self.init.state(), SegmentState::Loaded)
    }

    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        u32::try_from(self.segments.len()).unwrap_or(u32::MAX)
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

    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.range_ready(&range) {
            return SourcePhase::Ready;
        }
        if self.timeline.is_flushing() {
            return SourcePhase::Seeking;
        }
        let total = self.total_bytes();
        if total > 0 && range.start >= total {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    pub(crate) fn prefetch_anchor(&self) -> u64 {
        self.prefetch_anchor.load(Ordering::Acquire)
    }

    pub(crate) fn range_ready(&self, range: &Range<u64>) -> bool {
        let total = self.total_bytes();
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return true;
        }

        let mut cursor = range.start;
        while let Some((ref key, init_range)) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.asset_store.contains_range(key, local_start..local_end) {
                return false;
            }
            cursor = slice_end;
            if cursor >= end {
                return true;
            }
        }
        if cursor >= end {
            return true;
        }

        while cursor < end {
            let Some((ref key, seg_off, seg_size)) = self.media_descriptor(cursor) else {
                return false;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self.asset_store.contains_range(key, local_start..local_end) {
                return false;
            }
            cursor = slice_end;
        }
        cursor >= end
    }

    #[kithara::hang_watchdog]
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let total = self.total_bytes();
        if total > 0 && offset >= total {
            return Ok(ReadOutcome::Eof);
        }

        let buf_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let mut written: usize = 0;
        let mut cursor = offset;
        let read_end = offset.saturating_add(buf_len);

        while let Some((ref key, init_range)) = self.init_descriptor_at(cursor) {
            hang_tick!();
            if cursor >= init_range.end {
                break;
            }
            let slice_end = read_end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.read_resource(key, local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        return Ok(Self::wrap(written));
                    }
                    if cursor >= read_end {
                        return Ok(Self::wrap(written));
                    }
                }
                None => return Ok(Self::wrap(written)),
            }
        }

        while cursor < read_end {
            hang_tick!();
            let Some((key, seg_off, seg_size)) = self.media_descriptor(cursor) else {
                break;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = read_end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.read_resource(&key, local_start..local_end, dst)? {
                Some(n) => {
                    written += n;
                    cursor += n as u64;
                    if n < take {
                        break;
                    }
                }
                None => break,
            }
        }

        Ok(Self::wrap(written))
    }

    fn read_resource(
        &self,
        key: &ResourceKey,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        let resource = match self.asset_store.open_resource(key) {
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

    /// Replace the cancel token with a fresh child of `master_cancel`.
    /// Called on every re-activation path ([`Self::reset_to_full_range`]
    /// and [`Self::activate_at_segment_with_shift`]) so a variant that
    /// was deactivated (cancelled) on a prior ABR commit can dispatch
    /// fetches again. Without this, the second activation of the same
    /// `HlsVariant` instance enqueues `FetchCmd`s under a permanently
    /// cancelled token — every fetch settles immediately as
    /// `stale (cancelled)` and the reader hangs on `wait_range`.
    /// In-flight clones held by prior fetches stay cancelled (correct —
    /// they belong to the previous epoch and must not write).
    pub(crate) fn rearm_cancel(&self) {
        let fresh = self.master_cancel.child_token();
        *self.cancel.lock_sync_write() = fresh;
    }

    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn rebuild(&self, _ctx: &PlanCtx, from_seg: u32) {
        let segs_len = self.num_segments();
        let init = self
            .needs_init_fetch()
            .then_some(PlannedFetch::Init)
            .into_iter();
        let tail = (from_seg..segs_len).map(PlannedFetch::Segment);
        let mut queue = self.queue.lock_sync();
        queue.clear();
        queue.extend(init.chain(tail));
    }

    pub(crate) fn rebuild_at_time(&self, ctx: &PlanCtx, target: Duration) -> Option<u32> {
        let seg = self.segment_index_at_time(target)?;
        if let Some(byte) = self.segment_byte_offset(seg) {
            self.set_prefetch_anchor(byte);
        }
        self.rebuild(ctx, seg);
        Some(seg)
    }

    /// Same as [`Self::rebuild`] but **also** enqueues `seg 0` when
    /// `from_seg > 0`. The decoder factory's probe (Symphonia format
    /// reader) reads the container's first ~1 KB to construct the
    /// codec; on post-ABR switch into mid-playback the active variant's
    /// queue would otherwise start at `target_seg`, leaving
    /// `[0..PROBE)` unfetched and the probe hanging on
    /// `wait_range budget exceeded v=new range_start=0` (Cluster C/D/F
    /// sentinel, Wave 2.A.3b memo).
    ///
    /// `seg 0` is required even when the variant advertises a separate
    /// init (CMAF `EXT-X-MAP`): the init covers a few-dozen-byte
    /// header, but the probe scans further into the first media chunk.
    /// Skipping `seg 0` here masked the bug while `rebuild(0)` worked
    /// (loaded every segment from 0 — over-fetched whole file).
    ///
    /// After probe succeeds, `decoder_seek_safe(target_time)` jumps the
    /// decoder forward to the user-visible position, so segments
    /// `1..from_seg` are **never** fetched — only `seg 0` itself.
    /// Adds at most one extra segment (~`one_segment_size` bytes) per
    /// switch; if `seg 0` is already cached the scheduler skips the
    /// fetch and the queue entry resolves via `committed_final_len` in
    /// `dispatch`.
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock_sync().len() as u64
    )]
    pub(crate) fn rebuild_with_decoder_probe(&self, _ctx: &PlanCtx, from_seg: u32) {
        let segs_len = self.num_segments();
        let init = self
            .needs_init_fetch()
            .then_some(PlannedFetch::Init)
            .into_iter();
        let probe_seg = (from_seg > 0)
            .then_some(PlannedFetch::Segment(0))
            .into_iter();
        let tail = (from_seg..segs_len).map(PlannedFetch::Segment);
        let mut queue = self.queue.lock_sync();
        queue.clear();
        queue.extend(init.chain(probe_seg).chain(tail));
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
        let seed = self.init_seed.load(Ordering::Acquire);
        let mut cum = if seed > 0 { seed } else { self.init_size() };
        for (i, s) in self.segments.iter().enumerate() {
            offsets[i] = cum;
            cum += s.size.load(Ordering::Acquire);
        }
    }

    /// Reset variant to a "fresh" single-variant layout: `byte_shift = 0`,
    /// `served_from = 0`, `served_until = num_segments`. Called from
    /// [`HlsCoord::reset_for_seek`] so a random seek collapses the
    /// cross-variant byte continuity layering — variants archived from
    /// earlier auto-switches no longer co-serve the byte address space,
    /// and the (single) active variant addresses its segments by their
    /// natural offsets. Subsequent ABR commits at boundary will re-build
    /// the layering as usual.
    pub(crate) fn reset_to_full_range(&self) {
        self.byte_shift.store(0, Ordering::Release);
        self.served_from.store(0, Ordering::Release);
        self.served_until
            .store(self.num_segments(), Ordering::Release);
        self.recompute_offsets();
        self.rearm_cancel();
    }

    pub(crate) fn seek_time_anchor(
        &self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>> {
        let variant = self.variant;
        let Some((segment_index, segment_start, segment_end)) = self
            .playlist_state
            .find_seek_point_for_time(variant, position)
        else {
            return Err(StreamError::Source(
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
                .into(),
            ));
        };
        let byte_offset = self
            .segment_byte_offset(segment_index.try_into().unwrap_or(u32::MAX))
            .or_else(|| {
                self.playlist_state
                    .segment_byte_offset(variant, segment_index)
            })
            .ok_or_else(|| {
                StreamError::Source(
                    HlsError::SegmentNotFound(format!(
                        "seek offset not found: variant={variant} segment={segment_index}"
                    ))
                    .into(),
                )
            })?;
        let seg_idx_u32 = u32::try_from(segment_index).unwrap_or(u32::MAX);
        let anchor = SourceSeekAnchor::builder()
            .byte_offset(byte_offset)
            .segment_start(segment_start)
            .segment_end(segment_end)
            .segment_index(seg_idx_u32)
            .variant_index(variant)
            .build();
        self.set_position(byte_offset);
        self.wake_peer();
        Ok(Some(anchor))
    }

    /// Virtual byte offset of segment `seg_idx` in the combined stream.
    /// For the initial variant (`byte_shift == 0`) this equals the natural
    /// offset; after an Auto-mode switch this places the segment relative
    /// to the reader's current byte position at the switch boundary.
    pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64> {
        let natural = self.segment_byte_offset_natural(seg_idx)?;
        let shift = self.byte_shift();
        let virt = i64::try_from(natural).ok()?.checked_add(shift)?;
        u64::try_from(virt).ok()
    }

    /// Natural byte offset of segment `seg_idx` — i.e. without applying
    /// `byte_shift`. Used internally by `activate_*` to compute the
    /// shift needed to pin a segment at a given virtual byte.
    pub(crate) fn segment_byte_offset_natural(&self, seg_idx: u32) -> Option<u64> {
        self.segment_byte_offset_usize(seg_idx as usize)
    }

    fn segment_byte_offset_usize(&self, idx: usize) -> Option<u64> {
        self.offsets.lock_sync_read().get(idx).copied()
    }

    pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        let idx = idx.min(self.segments.len() - 1);
        u32::try_from(idx).ok()
    }

    pub(crate) fn segment_resource(&self, seg_idx: u32) -> Option<ResourceKey> {
        self.segments
            .get(seg_idx as usize)
            .map(|e| e.resource_id.clone())
    }

    /// Media size of segment `idx` — pure media only; the init prefix
    /// (when present) lives in its own range `[0, init.size)` and is
    /// served by the source via [`Self::init_resource`].
    fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.size.load(Ordering::Acquire))
    }

    pub(crate) fn served_from(&self) -> u32 {
        self.served_from.load(Ordering::Acquire)
    }

    pub(crate) fn served_until(&self) -> u32 {
        self.served_until.load(Ordering::Acquire)
    }

    /// Install the wake handle that the owning peer listens on. Called
    /// once by `HlsCoord` after the peer is bound. Subsequent calls
    /// silently keep the first registration.
    pub(crate) fn set_peer_wake(&self, notify: Arc<Notify>) {
        let _ = self.peer_wake.set(notify);
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn set_prefetch_anchor(&self, byte: u64) {
        self.prefetch_anchor.store(byte, Ordering::Release);
    }

    /// Cap the upper bound (exclusive) of segments this variant serves.
    /// Called from [`HlsCoord::commit_variant_switch`] on same-codec ABR
    /// commit so the outgoing variant's `find_at_offset` returns `None`
    /// for segments at or past the boundary — gates the reader's
    /// `SegmentReadStart` events against the post-switch range owned by
    /// the incoming variant, preventing a duplicate `(v_old, from_seg)`
    /// emit when the reader cursor lingers in the boundary segment.
    pub(crate) fn set_served_until(&self, until: u32) {
        self.served_until.store(until, Ordering::Release);
    }

    #[kithara::probe(variant = self.variant as u64, total = self.total_bytes_inner())]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes_inner()
    }

    fn total_bytes_inner(&self) -> u64 {
        let offsets = self.offsets.lock_sync_read();
        let last_idx = (self.served_until() as usize).saturating_sub(1);
        let natural = if let (Some(off), Some(_seg)) =
            (offsets.get(last_idx).copied(), self.segments.get(last_idx))
        {
            off + self.segment_size(last_idx).unwrap_or(0)
        } else if let Some(idx) = self.segments.len().checked_sub(1)
            && let Some(off) = offsets.get(idx).copied()
        {
            off + self.segment_size(idx).unwrap_or(0)
        } else {
            self.init_size()
        };
        let shift = self.byte_shift();
        let adjusted = i64::try_from(natural)
            .ok()
            .and_then(|n| n.checked_add(shift));
        match adjusted {
            Some(v) if v >= 0 => u64::try_from(v).unwrap_or(0),
            _ => 0,
        }
    }

    #[kithara::hang_watchdog]
    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let started_at = Instant::now();
        loop {
            if self.range_ready(&range) {
                hang_reset!();
                return Ok(WaitOutcome::Ready);
            }
            if self.timeline.is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
            let total = self.total_bytes();
            if total > 0 && range.start >= total {
                return Ok(WaitOutcome::Eof);
            }
            if let Some(budget) = timeout
                && started_at.elapsed() > budget
            {
                warn!(
                    target: "kithara_hls::wait",
                    v = self.variant,
                    range_start = range.start,
                    range_end = range.end,
                    range_len = range.end.saturating_sub(range.start),
                    budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
                    "wait_range budget exceeded"
                );
                return Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()));
            }
            self.wake_peer();
            hang_tick!();
            sleep(Duration::from_millis(2));
        }
    }

    pub(crate) fn wake_peer(&self) {
        if let Some(notify) = self.peer_wake.get() {
            notify.notify_one();
        }
    }

    fn wrap(written: usize) -> ReadOutcome {
        NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        )
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
    state: Arc<AtomicU8>,
    resource: AssetResource<DecryptContext>,
    cancel: CancellationToken,
    planned: PlannedFetch,
    variant: Weak<HlsVariant>,
}

impl From<FetchSlot> for OnCompleteFn {
    fn from(slot: FetchSlot) -> Self {
        Box::new(move |bytes_written, _headers, err| slot.settle(bytes_written, err))
    }
}

impl FetchSlot {
    /// On success, commits the resource. `bytes_written` is forwarded as
    /// `final_len` — required by [`ProcessedResource::commit`] to trigger
    /// the post-write decrypt pass on encrypted segments (passing `None`
    /// silently skips decryption, leaving ciphertext on disk). After a
    /// successful commit we read back the resource's `final_len` and
    /// shrink the variant's layout to match: for DRM segments PKCS7
    /// strips up to 16 bytes off the encrypted size, so HEAD-based
    /// estimates are always upper bounds.
    fn settle(&self, bytes_written: u64, err: Option<&NetError>) {
        if self.cancel.is_cancelled() {
            self.settle_cancelled(bytes_written);
            return;
        }
        let next = err.map_or_else(
            || self.settle_success(bytes_written),
            |e| self.settle_failure(e),
        );
        self.state.store(next as u8, Ordering::Release);
    }

    fn settle_cancelled(&self, bytes_written: u64) {
        let committed = matches!(self.resource.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", bytes_written, committed, "stale (cancelled)");
        if !committed {
            self.resource
                .fail("fetch cancelled before completion".into());
            self.state
                .store(SegmentState::Missing as u8, Ordering::Release);
        }
    }

    fn settle_failure(&self, e: &NetError) -> SegmentState {
        let committed = matches!(self.resource.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", err = %e, committed, "fail-path");
        if committed {
            if let ResourceStatus::Committed { final_len: Some(n) } = self.resource.status()
                && let Some(v) = self.variant.upgrade()
            {
                v.apply_commit(self.planned, n);
            }
            SegmentState::Loaded
        } else {
            self.resource.fail(e.to_string());
            SegmentState::Missing
        }
    }

    fn settle_success(&self, bytes_written: u64) -> SegmentState {
        let commit_result = self.resource.commit(Some(bytes_written));
        let commit_ok = commit_result.is_ok();
        match &commit_result {
            Ok(()) => debug!(target: "kithara_hls::settle", commit_ok, bytes_written, "success"),
            Err(e) => debug!(
                target: "kithara_hls::settle",
                commit_ok,
                bytes_written,
                err = %e,
                "success-but-commit-failed"
            ),
        }
        if !commit_ok {
            return SegmentState::Missing;
        }
        let actual = match self.resource.status() {
            ResourceStatus::Committed { final_len: Some(n) } => n,
            _ => bytes_written,
        };
        if let Some(v) = self.variant.upgrade() {
            v.apply_commit(self.planned, actual);
        }
        SegmentState::Loaded
    }

    fn writer(&self) -> WriterFn {
        let resource = self.resource.clone();
        let offset = Arc::new(AtomicU64::new(0));
        Box::new(move |chunk: &[u8]| {
            let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            resource.write_at(pos, chunk).map_err(IoError::other)
        })
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
