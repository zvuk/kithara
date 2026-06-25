#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use kithara_assets::{
    AcquisitionResult, AssetResource, AssetScope, ReadSide, ResourceKey, WriteSide,
};
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Mutex, time::Duration};
use kithara_storage::ResourceStatus;
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, SeekObserve, SourceSeekAnchor, StreamError,
    StreamResult,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
    needs_exact_byte_sizes,
};
use kithara_test_utils::kithara;
use url::Url;

use crate::{
    HlsError,
    conv::FromWithParams,
    handle::ResourceHandle,
    playlist::{PlaylistAccess, PlaylistState},
    segment::{
        Downloading, FetchClaim, FetchSlot, Loaded, MediaSegment, PlannedFetch, Segment,
        SegmentContent, SegmentSize, SegmentSlotState,
    },
    signal::SizeSignal,
};

mod cancel_epoch;
mod descriptor;
mod dispatch;
mod init;
mod offsets;
mod read;
mod reader_runtime;
use cancel_epoch::CancelEpoch;
use offsets::{ActivateParams, Layout};
use reader_runtime::ReaderRuntime;

#[cfg(test)]
mod tests;

const INIT_PLACEHOLDER_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy)]
struct SeekAlias {
    segment: u32,
    anchor: u64,
}

pub(crate) struct PlanCtx {
    /// Unified reader-wake handle (owned by [`HlsCoord`]). Every `FetchCmd` this
    /// plan emits fires it on each segment byte write and on settle
    /// (commit/fail) — [`SizeSignal::fire`] wakes both the off-RT reader parked
    /// in `wait_range(_, None)` (the moment its range fills, no wall-clock poll)
    /// and the RT decoder's audio worker (re-ticked on data arrival rather than
    /// on its 10 ms scheduler poll). The late-bound worker wake inside is filled
    /// once by `HlsSource::set_worker_wake`; only the two downloader-thread sites
    /// fire it — the coord's RT-reachable fence/seek signals use
    /// [`SizeSignal::fire_ready_only`].
    pub(crate) signal: SizeSignal,
    pub(crate) scope: AssetScope<DecryptContext>,
    pub(crate) master_cancel: CancelToken,
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
    /// Snapshot of `SeekObserve::epoch()` at plan-time. Tagged on
    /// every emitted `FetchCmd`'s probe so integration tests can
    /// distinguish fetches that pre-date a user seek from those that
    /// the scheduler issued *after* observing the new epoch.
    pub(crate) seek_epoch: u64,
    pub(crate) prefetch_budget: usize,
}

/// Inputs to [`HlsVariant::activate_at_segment_with_shift`]: pin `from_seg`
/// at `seg_boundary` in virtual byte space and move the reader to
/// `reader_pos`.
#[derive(Clone, Copy)]
pub(crate) struct SegmentActivateParams {
    pub(crate) from_seg: u32,
    pub(crate) reader_pos: u64,
    pub(crate) seg_boundary: u64,
}

pub(crate) struct HlsVariant {
    /// Parsed master/media-playlist data. Owned by `Arc` so multiple
    /// variants share a single immutable view; used by
    /// `seek_time_anchor` (`find_seek_point_for_time`) and as the
    /// source for the cached [`Self::codec`] / [`Self::container`].
    playlist_state: Arc<PlaylistState>,
    /// Per-track asset store. The single source-of-truth clone: each vended
    /// [`ResourceHandle`] gets a cheap clone of it, and the fetch path
    /// acquires *writable* resources via the same clone in `PlanCtx::scope`.
    /// Vends a narrow `ResourceHandle` per segment/init (`segment_handle` /
    /// `init_handle`); the produce-core's disk read and acquire flow through
    /// that handle, and it is the home for the `WS5d` held-resource lease.
    scope: AssetScope<DecryptContext>,
    segment_aware_seek_tail: AtomicU32,
    prefetch_anchor: AtomicU64,
    /// The variant's cancel epoch: the per-track parent (mirror of
    /// `coord.cancel` = `PlanCtx::master_cancel`) plus the rotating
    /// per-activation child. Survives variant re-activation — a cross-codec
    /// `commit_variant_switch` may flip from `v_old` to `v_new` and back to
    /// `v_old`, and the second activation of `v_old` must dispatch fetches
    /// under a *live* cancel, so [`Self::rearm_cancel`] rotates the child.
    cancel_epoch: CancelEpoch,
    /// Coherent owner of the cross-variant byte-address-space coordinates
    /// (`byte_shift`, `served_from`, `served_until`, `init_seed`, the media
    /// offset table). A single lock guards all five so a reader never mixes
    /// the shift of one activation with the served bounds of the next.
    layout: Layout,
    queue: Mutex<VecDeque<PlannedFetch>>,
    seek_alias: Mutex<Option<SeekAlias>>,
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
    /// Init slot: `Some(Segment::Init)` for a variant that advertises a
    /// separately fetched `#EXT-X-MAP` init, `None` otherwise. Its existence
    /// is keyed on the playlist `#EXT-X-MAP` URL, never on the init HEAD size
    /// — see [`Self::build_init_entry`].
    init: Option<Segment>,
    /// Reader-side runtime: the shared byte cursor, peer wake handle, and the
    /// timeline consulted for `is_flushing` gating. The probe-tagged
    /// `get_position` / `set_position` stay on the facade and delegate here.
    reader: ReaderRuntime,
    /// Media entry table — the fetch path and descriptor builders index it
    /// for `url` / `content` / `decode_time`.
    segments: Vec<Segment>,
    variant: usize,
}

/// Media payload + shared-dep snapshot for [`HlsVariant::from_parts`].
/// Pulled out of the constructor's argument list so the call site stays
/// readable — `new()` builds this from parsed playlist metadata, tests
/// build it inline from synthesised fixtures.
pub(crate) struct VariantParts {
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
    pub(crate) codec: Option<AudioCodec>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) init: Option<Segment>,
    pub(crate) segments: Vec<Segment>,
}

/// In-memory init prefix size: 0 when the variant carries no separately
/// fetched `#EXT-X-MAP` init (`None`), else the init `Segment`'s current
/// (possibly post-commit shrunk) size atom.
fn init_size_of(init: &Option<Segment>) -> u64 {
    init.as_ref().map_or(0, Segment::len)
}

/// Route-only non-exact media placeholder for segment-aware fMP4. `EXTINF`
/// shapes a deliberately small synthetic byte step; `BANDWIDTH` can only lower
/// it because master ladder bandwidths are often peak/variant labels rather
/// than actual audio payload sizes. The exactness gate still prevents reads
/// until the segment body commit publishes `final_len`, so this must optimize
/// for stable routing geometry, not payload-size prediction.
fn segment_placeholder_size(duration: Duration, bandwidth_bps: Option<u64>) -> u64 {
    const MIN_BYTES: u64 = 4 * 1024;
    const MAX_PRECOMMIT_BYTES: u64 = 256 * 1024;
    const PLACEHOLDER_BYTES_PER_SEC: u128 = 4 * 1024;
    const NANOS_PER_SEC: u128 = 1_000_000_000;
    const BITS_PER_BYTE: u128 = 8;

    let duration_nanos = duration.as_nanos();
    if duration_nanos == 0 {
        return MIN_BYTES;
    }
    let duration_bytes = duration_nanos
        .saturating_mul(PLACEHOLDER_BYTES_PER_SEC)
        .div_ceil(NANOS_PER_SEC);
    let estimated_bytes = bandwidth_bps
        .filter(|bps| *bps > 0)
        .map_or(duration_bytes, |bps| {
            let bandwidth_bytes = u128::from(bps)
                .saturating_mul(duration_nanos)
                .div_ceil(BITS_PER_BYTE * NANOS_PER_SEC);
            duration_bytes.min(bandwidth_bytes)
        });
    u64::try_from(estimated_bytes)
        .unwrap_or(u64::MAX)
        .clamp(MIN_BYTES, MAX_PRECOMMIT_BYTES)
}

/// Per-variant construction parameters: the runtime context a parsed
/// [`PlaylistState`] cannot carry, folded in via [`FromWithParams`].
/// `decrypt_contexts[i]` carries the pre-resolved [`DecryptContext`] for
/// segment `i` (or `None` for cleartext segments) — the caller resolves
/// AES-128 keys through [`KeyStore`](crate::playlist::KeyStore) before
/// construction.
pub(crate) struct VariantParams<'a> {
    pub(crate) variant_idx: usize,
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
    pub(crate) init_decrypt_ctx: Option<DecryptContext>,
    pub(crate) decrypt_contexts: &'a [Option<DecryptContext>],
    pub(crate) ctx: &'a PlanCtx,
}

/// Production constructor: read parsed playlist metadata and assemble the
/// per-variant index, init/segment entries, queue, and cancel hierarchy.
impl FromWithParams<&Arc<PlaylistState>, VariantParams<'_>> for Arc<HlsVariant> {
    fn build(playlist_state: &Arc<PlaylistState>, params: VariantParams<'_>) -> Self {
        let VariantParams {
            variant_idx,
            seek_obs,
            init_decrypt_ctx,
            decrypt_contexts,
            ctx,
        } = params;
        let init = HlsVariant::build_init_entry(
            playlist_state.as_ref(),
            variant_idx,
            init_decrypt_ctx,
            &ctx.scope,
        );
        let segments = HlsVariant::build_segment_entries(
            playlist_state.as_ref(),
            decrypt_contexts,
            variant_idx,
            init_size_of(&init),
            &ctx.scope,
        );
        let codec = playlist_state.variant_codec(variant_idx);
        let container = playlist_state.variant_container(variant_idx);
        HlsVariant::from_parts(
            variant_idx,
            VariantParts {
                codec,
                container,
                init,
                segments,
                seek_obs,
                playlist_state: Arc::clone(playlist_state),
            },
            ctx,
        )
    }
}

impl HlsVariant {
    const NO_SEEK_TAIL: u32 = u32::MAX;

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
        params: SegmentActivateParams,
    ) {
        let SegmentActivateParams {
            from_seg,
            seg_boundary,
            reader_pos,
        } = params;
        self.rearm_cancel();
        let from_seg = from_seg.min(self.num_segments());
        self.layout.activate_with_shift(
            ActivateParams {
                from_seg,
                seg_boundary,
                init_size: self.init_size(),
            },
            &self.segments,
        );
        self.set_position(reader_pos);
        self.rebuild(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.reader.advance(n);
        if !self.reader.is_seek_active() {
            self.clear_seek_alias_if_moved(self.reader.position());
        }
    }

    /// Settle hook: shrinks the appropriate size atom to `actual` and
    /// rebuilds the offset map. Called from
    /// [`FetchSlot::settle`] via `Weak<HlsVariant>::upgrade()` once the
    /// resource commits — for DRM, this is where the post-PKCS7 length
    /// replaces the encrypted estimate.
    ///
    /// Size store and offset recompute happen under the same Layout write
    /// lock — a reader that races in between would see a new size with
    /// stale offsets and fall into a non-existent gap, hanging on
    /// `range_ready`. The closure performs the caller-owned size store and
    /// reports the post-store `init_size` to seed the recompute.
    pub(crate) fn apply_commit(&self, loaded: &FetchClaim<Loaded>) {
        self.layout.apply_commit(&self.segments, || {
            self.apply_loaded_size(loaded.planned(), loaded.final_len());
            self.init_size()
        });
    }

    /// Common assembly for init and segment fetches. Both go through the
    /// same `FetchSlot`: writer streams to the asset resource, `on_complete`
    /// runs `settle` which observes `cancel.is_cancelled()` as the epoch
    /// gate.
    fn build_cmd(
        self: &Arc<Self>,
        url: Url,
        acq: AssetResource<DecryptContext>,
        handle: FetchClaim<Downloading>,
        signal: SizeSignal,
    ) -> Option<FetchCmd> {
        let writer = match acq {
            AcquisitionResult::Pending(writer) => writer,
            // Committed between the skip-fetch probe and acquire — no download.
            // Mirror `settle_success`: mark the segment loaded at its on-disk
            // length so announced/estimated sizes match the bytes on disk.
            AcquisitionResult::Ready(reader) => {
                match reader.status() {
                    ResourceStatus::Committed { final_len: Some(n) } => {
                        handle.into_loaded(n);
                    }
                    _ => {
                        handle.into_loaded_no_apply();
                    }
                }
                signal.fire();
                return None;
            }
            _ => {
                handle.into_missing();
                return None;
            }
        };
        let cancel = self.cancel_handle();
        let slot = FetchSlot {
            handle,
            reader: writer.reader(),
            raw: writer.raw_write_handle(),
            writer,
            cancel: cancel.clone(),
            signal: signal.clone(),
        };
        let mut inner_writer = slot.writer();
        // Per-chunk write signal: wake a reader parked in `wait_range(_, None)`
        // the moment bytes land (not only on commit), so a sub-segment range
        // resolves without waiting for settle. Also re-ticks the RT decoder's
        // audio worker (off the 10 ms scheduler poll) the instant plaintext
        // bytes land. Runs on the downloader thread (off-RT) — taking the
        // gate's condvar mutex and the wait-free worker unpark are allowed here.
        let writer_fn: WriterFn = Box::new(move |chunk: &[u8]| {
            let result = inner_writer(chunk);
            if result.is_ok() {
                signal.fire();
            }
            result
        });
        Some(
            FetchCmd::get(url)
                .cancel(cancel)
                .maybe_headers(self.headers.clone())
                .writer(writer_fn)
                .on_complete(OnCompleteFn::from(slot))
                .build(),
        )
    }

    /// Builds per-segment metadata. Segment 0's `segment_byte_offset` from
    /// the playlist absorbs the init prefix (`size_map` convention) — we
    /// subtract `init_size` so each [`MediaSegment::size`] holds the
    /// encrypted **media-only** length; subsequent segments are pure
    /// media. `init_size` is the in-memory `AtomicU64` value used as the
    /// running seed for cumulative offsets.
    fn build_segment_entries(
        playlist_state: &PlaylistState,
        decrypt_contexts: &[Option<DecryptContext>],
        variant_idx: usize,
        init_size: u64,
        scope: &AssetScope<DecryptContext>,
    ) -> Vec<Segment> {
        let Some(num) = playlist_state.num_segments(variant_idx) else {
            return Vec::new();
        };
        let needs_exact = needs_exact_byte_sizes(
            playlist_state.variant_codec(variant_idx),
            playlist_state.variant_container(variant_idx),
        );
        let use_placeholder = !needs_exact && !playlist_state.has_size_map(variant_idx);
        let bandwidth_bps = playlist_state.variant_bandwidth_bps(variant_idx);
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
            let size = if use_placeholder {
                SegmentSize::placeholder(segment_placeholder_size(duration, bandwidth_bps))
            } else {
                SegmentSize::seed(media_size)
            };
            entries.push(Segment::Media(MediaSegment {
                resource_id: scope.key_from_url(&url),
                url,
                state: SegmentSlotState::missing(),
                size,
                content: SegmentContent::from(decrypt_ctx),
                decode_time,
                duration,
            }));
            decode_time = decode_time.saturating_add(duration);
        }
        entries
    }

    pub(crate) fn cancel(&self) {
        self.cancel_epoch.cancel();
    }

    pub(crate) fn cancel_handle(&self) -> CancelToken {
        self.cancel_epoch.handle()
    }

    /// Index of the first non-`Loaded` segment — interpreted as the
    /// "download head" by the ABR controller. Returns `num_segments()`
    /// when every segment is `Loaded`. Scans linearly; cheap because it
    /// only runs from `Abr::progress` (ABR tick cadence).
    pub(crate) fn download_head(&self) -> u32 {
        let head = self
            .segments
            .iter()
            .position(|s| !s.state().is_loaded())
            .unwrap_or(self.segments.len());
        u32::try_from(head).unwrap_or(u32::MAX)
    }

    /// Reader-facing lookup in **virtual** byte space — delegates to the
    /// [`Layout`], which subtracts `byte_shift`, runs the natural-space
    /// search, and gates against `[served_from..served_until)` under one
    /// lock. Returns `None` when the byte falls outside the served range so
    /// cross-variant lookups in [`HlsCoord::find_at_offset`] fall through to
    /// the previous variant.
    #[kithara::probe(
        variant = self.variant as u64,
        byte_offset,
        found_seg = self
            .layout
            .find_at_offset(byte_offset, &self.segments)
            .map_or(u64::MAX, |(i, _, _)| u64::from(i))
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.seek_alias_at(byte_offset)
            .or_else(|| self.layout.find_at_offset(byte_offset, &self.segments))
    }

    /// Bare assembly used by unit tests inside this module.
    #[must_use]
    fn from_parts(variant: usize, parts: VariantParts, ctx: &PlanCtx) -> Arc<Self> {
        let VariantParts {
            playlist_state,
            init,
            codec,
            container,
            seek_obs,
            segments,
        } = parts;
        let init_size = init_size_of(&init);
        let layout = Layout::new(init_size, &segments);
        Arc::new(Self {
            variant,
            init,
            segments,
            playlist_state,
            layout,
            codec,
            container,
            scope: ctx.scope.clone(),
            reader: ReaderRuntime::new(seek_obs),
            seek_alias: Mutex::default(),
            segment_aware_seek_tail: AtomicU32::new(Self::NO_SEEK_TAIL),
            prefetch_anchor: AtomicU64::new(0),
            queue: Mutex::default(),
            cancel_epoch: CancelEpoch::new(ctx.master_cancel.clone()),
            headers: ctx.headers.clone(),
        })
    }

    #[kithara::probe(variant = self.variant as u64, pos = self.reader.position())]
    pub(crate) fn get_position(&self) -> u64 {
        self.reader.position()
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
    /// post-commit inits at the `ByteMap` boundary.
    #[kithara::probe(variant = self.variant as u64, size = self.init_size())]
    pub(crate) fn init_byte_range(&self) -> Range<u64> {
        0..self.init_size()
    }

    /// Coherent "is this variant historical?" check — `served_from` and
    /// `served_until` read under a single Layout lock.
    pub(crate) fn is_shrunk(&self) -> bool {
        self.layout.is_shrunk(self.num_segments())
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

    pub(crate) fn prefetch_anchor(&self) -> u64 {
        self.prefetch_anchor.load(Ordering::Acquire)
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
        self.cancel_epoch.rearm();
    }

    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock().len() as u64
    )]
    pub(crate) fn rebuild(&self, _ctx: &PlanCtx, from_seg: u32) {
        self.rebuild_queue(from_seg);
    }

    fn rebuild_queue(&self, from_seg: u32) {
        let segs_len = self.num_segments();
        let init = self
            .needs_init_fetch()
            .then_some(PlannedFetch::Init)
            .into_iter();
        let tail = (from_seg..segs_len).map(PlannedFetch::Segment);
        let mut queue = self.queue.lock();
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

    /// Same as [`Self::rebuild`] but also enqueues `seg 0` when
    /// `from_seg > 0`, so the decoder factory's probe has the container
    /// header to construct the codec. See the crate `CONTEXT.md`
    /// "Decoder-probe rebuild".
    #[kithara::probe(
        variant = self.variant as u64,
        from_seg,
        old_queue_len = self.queue.lock().len() as u64
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
        let mut queue = self.queue.lock();
        queue.clear();
        queue.extend(init.chain(probe_seg).chain(tail));
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
        self.clear_seek_alias();
        self.clear_segment_aware_seek_tail();
        self.reset_layout_to_full_range();
        self.rearm_cancel();
    }

    fn reset_layout_to_full_range(&self) {
        self.layout.reset(self.init_size(), &self.segments);
    }

    /// Seek reset is layout-only. Active body fetches stay live: segment-aware
    /// decoders re-resolve media ranges by segment index, and canceling the
    /// in-flight target segment would put a streaming seek behind tail prefetch.
    pub(crate) fn reset_for_seek(&self) {
        self.reset_layout_to_full_range();
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
        self.set_prefetch_anchor(byte_offset);
        self.set_seek_alias(byte_offset, seg_idx_u32);
        self.set_segment_aware_seek_tail(seg_idx_u32);
        self.rebuild_queue(seg_idx_u32);
        Ok(Some(anchor))
    }

    /// Virtual byte offset of segment `seg_idx` in the combined stream.
    /// For the initial variant (`byte_shift == 0`) this equals the natural
    /// offset; after an Auto-mode switch this places the segment relative
    /// to the reader's current byte position at the switch boundary.
    pub(crate) fn segment_byte_offset(&self, seg_idx: u32) -> Option<u64> {
        self.layout.segment_byte_offset(seg_idx)
    }

    /// Natural byte offset of segment `seg_idx` — i.e. without applying
    /// `byte_shift`. Used internally by `activate_*` to compute the
    /// shift needed to pin a segment at a given virtual byte.
    pub(crate) fn segment_byte_offset_natural(&self, seg_idx: u32) -> Option<u64> {
        self.layout.natural_offset(seg_idx as usize)
    }

    pub(crate) fn segment_index_at_time(&self, t: Duration) -> Option<u32> {
        self.index_at_time(t)
            .and_then(|idx| u32::try_from(idx).ok())
    }

    pub(crate) fn served_from(&self) -> u32 {
        self.layout.served_from()
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
        if !self.reader.is_seek_active() {
            self.clear_seek_alias_if_moved(pos);
        }
        self.reader.set_position(pos);
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
        self.layout
            .set_served_until(until, &self.segments, self.init_size());
    }

    /// Whether every served segment's byte size is known. While `false`,
    /// [`Self::total_bytes`] is a lower bound (a segment's size estimate is
    /// missing), so the byte-EOF gates must hold `Waiting`/`Pending` rather
    /// than mint EOF for an in-range offset that only looks past-the-end
    /// against the under-count.
    pub(crate) fn sizes_complete(&self) -> bool {
        self.layout.sizes_complete()
    }

    pub(crate) fn eof_ready(&self) -> bool {
        self.sizes_complete() || self.segment_aware_seek_tail_complete()
    }

    #[kithara::probe(
        variant = self.variant as u64,
        total = self.layout.total_bytes()
    )]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.layout.total_bytes()
    }

    /// Settle-side size store: shrink the appropriate atom to `final_len`.
    /// The caller runs this inside [`Layout::apply_commit`](
    /// offsets::Layout::apply_commit)'s write-lock so a reader never
    /// observes a new size against a stale offset table.
    fn apply_loaded_size(&self, planned: PlannedFetch, final_len: u64) {
        match planned {
            PlannedFetch::Init => {
                // Only a `Some(Init)` slot is ever settled (it is the only init
                // that gets fetched). A `None` init has no size atom; a stray
                // settle is a no-op rather than resurrecting an init.
                if let Some(init) = self.init.as_ref() {
                    init.set_loaded_size(final_len);
                }
            }
            PlannedFetch::Segment(idx) => {
                if let Some(slot) = self.segments.get(idx as usize) {
                    slot.set_loaded_size(final_len);
                }
            }
        }
    }

    /// Committed on-disk length for media segment `seg_idx` when its resource
    /// is `Committed` with a known `final_len` — the skip-fetch guard's size
    /// source. `None` when the index is out of range.
    fn committed_final_len(&self, seg_idx: u32) -> Option<u64> {
        self.segments
            .get(seg_idx as usize)?
            .committed_len(&self.scope)
    }

    /// Whether every byte in `range` is already present on disk for media
    /// segment `seg_idx` — routed through the [`Segment`] cascade.
    fn segment_contains(&self, seg_idx: u32, range: Range<u64>) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|seg| seg.size().is_exact() && seg.contains(&self.scope, range))
    }

    /// Read `range` of media segment `seg_idx` into `dst` via the [`Segment`]
    /// cascade. `Ok(None)` when the segment is out of range or its bytes are
    /// not on disk yet.
    fn segment_read_at(
        &self,
        seg_idx: u32,
        range: Range<u64>,
        dst: &mut [u8],
    ) -> StreamResult<Option<usize>> {
        self.segments.get(seg_idx as usize).map_or_else(
            || Ok(None),
            |seg| {
                if seg.size().is_exact() {
                    seg.read_at(&self.scope, range, dst)
                } else {
                    Ok(None)
                }
            },
        )
    }

    /// Clamped segment index whose decode-time window contains `t`, or
    /// `None` when the variant has no segments.
    fn index_at_time(&self, t: Duration) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        let idx = bisect_right_decode_time(&self.segments, t).saturating_sub(1);
        Some(idx.min(self.segments.len() - 1))
    }

    fn segment_downloading(&self, seg_idx: u32) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|s| s.state().is_downloading())
    }

    fn clear_seek_alias(&self) {
        *self.seek_alias.lock() = None;
    }

    fn clear_segment_aware_seek_tail(&self) {
        self.segment_aware_seek_tail
            .store(Self::NO_SEEK_TAIL, Ordering::Release);
    }

    fn clear_seek_alias_if_moved(&self, pos: u64) {
        let mut alias = self.seek_alias.lock();
        if alias.is_some_and(|entry| entry.anchor != pos) {
            *alias = None;
        }
    }

    fn seek_alias_at(&self, byte: u64) -> Option<(u32, u64, u64)> {
        let alias = *self.seek_alias.lock();
        let alias = alias?;
        let size = self.segment_size(alias.segment as usize)?;
        let end = alias.anchor.saturating_add(size);
        (byte >= alias.anchor && byte < end).then_some((alias.segment, alias.anchor, size))
    }

    fn set_seek_alias(&self, anchor: u64, segment: u32) {
        *self.seek_alias.lock() = Some(SeekAlias { segment, anchor });
    }

    fn set_segment_aware_seek_tail(&self, segment: u32) {
        if !needs_exact_byte_sizes(self.codec, self.container) {
            self.segment_aware_seek_tail
                .store(segment, Ordering::Release);
        }
    }

    fn segment_aware_seek_tail_complete(&self) -> bool {
        if needs_exact_byte_sizes(self.codec, self.container) {
            return false;
        }
        let start = self.segment_aware_seek_tail.load(Ordering::Acquire);
        if start == Self::NO_SEEK_TAIL {
            return false;
        }
        let start = start as usize;
        let Some(tail) = self.segments.get(start..) else {
            return false;
        };
        !tail.is_empty() && tail.iter().all(|segment| segment.size().is_exact())
    }

    /// Whether media segment `seg_idx` settled terminally (`Failed`): the
    /// downloader exhausted its retry budget, so the segment will never
    /// load. Readers surface a terminal error on it.
    fn segment_failed(&self, seg_idx: u32) -> bool {
        self.segments
            .get(seg_idx as usize)
            .is_some_and(|s| s.state().is_failed())
    }

    /// Narrow disk handle for media segment `seg_idx`, or `None` when the
    /// index is out of range. Cheap: clones the shared scope plus the
    /// segment's key and url.
    fn segment_handle(&self, seg_idx: u32) -> Option<ResourceHandle> {
        Some(self.segments.get(seg_idx as usize)?.resource(&self.scope))
    }

    /// Media size of segment `idx` — pure media only; the init prefix lives
    /// in its own `[0, init.size)` range.
    fn segment_size(&self, idx: usize) -> Option<u64> {
        Some(self.segments.get(idx)?.len())
    }

    /// Borrow the media table — the fetch path and the descriptor builders
    /// index it for `url` / `content` / `decode_time`.
    #[cfg(test)]
    fn segments(&self) -> &[Segment] {
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
