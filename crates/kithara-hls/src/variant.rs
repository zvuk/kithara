#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_assets::{
    AcquisitionResult, AssetResource, AssetScope, ReadSide, ResourceKey, WriteSide,
};
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{CancelToken, sync::Mutex, time::Duration};
use kithara_storage::{ResourceStatus, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, PendingReason, ReadOutcome, SeekObserve,
    SegmentDescriptor, SourceError, SourcePhase, SourceSeekAnchor, StreamError, StreamResult,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    conv::FromWithParams,
    playlist::{PlaylistAccess, PlaylistState},
    segment::{
        Downloading, FetchClaim, FetchSlot, InitSegment, Loaded, MediaSegment, PlannedFetch,
        Segment, SegmentContent, SegmentSize, SegmentSlotState,
    },
    signal::SizeSignal,
};

mod cancel_epoch;
mod offsets;
mod reader_runtime;
mod segment_store;
use cancel_epoch::CancelEpoch;
use offsets::{ActivateParams, Layout};
use reader_runtime::ReaderRuntime;
use segment_store::SegmentStore;

#[cfg(test)]
mod tests;

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
    /// Reader-side runtime: the shared byte cursor, peer wake handle, and the
    /// timeline consulted for `is_flushing` gating. The probe-tagged
    /// `get_position` / `set_position` stay on the facade and delegate here.
    reader: ReaderRuntime,
    /// Segment/init content domain: the asset scope plus the init and
    /// media entry tables. It vends a narrow `ResourceHandle` per
    /// segment/init (`segment_handle` / `init_handle`); the produce-core's
    /// disk read and acquire flow through that handle, and it is the home
    /// for the `WS5d` held-resource lease. The fetch path on this facade
    /// borrows its `init()` / `segments()` to claim slots and build
    /// `FetchCmd`s.
    store: SegmentStore,
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
            self.store.segments(),
        );
        self.set_position(reader_pos);
        self.rebuild(ctx, from_seg);
    }

    #[kithara::probe(variant = self.variant as u64, n)]
    pub(crate) fn advance(&self, n: u64) {
        self.reader.advance(n);
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
        self.layout.apply_commit(self.store.segments(), || {
            self.store
                .apply_loaded_size(loaded.planned(), loaded.final_len());
            self.store.init_size()
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

    fn build_init_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        handle: FetchClaim<Downloading>,
    ) -> Option<FetchCmd> {
        let init = self.store.init()?;
        let resource_handle = self.store.init_handle()?;
        let resource = resource_handle
            .acquire(init.content())
            .expect("acquire_resource for init must succeed");
        self.build_cmd(
            resource_handle.url().clone(),
            resource,
            handle,
            ctx.signal.clone(),
        )
    }

    /// Builds the variant's init slot. The slot exists (`Some(Segment::Init)`)
    /// iff the playlist carries an `#EXT-X-MAP` URL — NOT iff the init HEAD
    /// produced a size (R5). A failed or absent init HEAD leaves
    /// `init_size() == 0` while the URL is present; the init is still a real
    /// segment that must be fetched (its committed `final_len` sets the real
    /// size). Keying existence on the HEAD size drops such an init, and
    /// `read_at(0)` then serves segment 0's container where the demuxer
    /// expects `ftyp` ("`re_mp4`: ftyp not found") or wedges with no progress.
    /// `None` is the old `VariantInit::NotApplicable`: no `#EXT-X-MAP`, or a
    /// byte-range-embedded init living in segment 0's byte range. See the crate
    /// `CONTEXT.md` "Variant init".
    fn build_init_entry(
        playlist_state: &PlaylistState,
        variant_idx: usize,
        decrypt_ctx: Option<DecryptContext>,
        scope: &AssetScope<DecryptContext>,
    ) -> Option<Segment> {
        let url = playlist_state.init_url(variant_idx)?;
        Some(Segment::Init(InitSegment {
            resource_id: scope.key_from_url(&url),
            url,
            state: SegmentSlotState::missing(),
            size: SegmentSize::seed(playlist_state.init_size(variant_idx)),
            content: SegmentContent::from(decrypt_ctx),
        }))
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
            entries.push(Segment::Media(MediaSegment {
                resource_id: scope.key_from_url(&url),
                url,
                state: SegmentSlotState::missing(),
                size: SegmentSize::seed(media_size),
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

    pub(crate) fn descriptor(&self, idx: usize) -> Option<SegmentDescriptor> {
        let entry = self.store.segments().get(idx)?.as_media()?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        let byte_offset = self.layout.natural_offset(idx)?;
        let size = self.store.segment_size(idx)?;
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
        if idx >= self.store.segments().len() {
            return None;
        }
        let off = self.layout.natural_offset(idx)?;
        if off < byte {
            idx += 1;
        }
        if idx >= self.store.segments().len() {
            return None;
        }
        self.descriptor(idx)
    }

    #[kithara::probe(variant = self.variant as u64, byte)]
    pub(crate) fn descriptor_at_byte(&self, byte: u64) -> Option<SegmentDescriptor> {
        let (idx, off, size) = self.find_at_offset(byte)?;
        let entry = self.store.segments().get(idx as usize)?.as_media()?;
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
        self.descriptor(self.store.index_at_time(t)?)
    }

    #[kithara::probe(
        variant = self.variant as u64,
        budget = budget as u64,
        queue_len = self.queue.lock().len() as u64
    )]
    #[kithara::hang_watchdog]
    pub(crate) fn dispatch(self: &Arc<Self>, ctx: &PlanCtx, budget: usize) -> Vec<FetchCmd> {
        let mut out = Vec::new();
        // Popped segments that could not be dispatched this pass but are NOT
        // terminal (a slot still `Downloading` under an orphaned/in-flight
        // fetch, or one that raced back to `Missing`): re-queued at the front
        // after the pass so a later dispatch re-claims them once the slot
        // frees. Without this, a seek that re-queues the target while an old
        // prefetch still holds it `Downloading` would pop+drop the target —
        // the orphaned fetch settles back to `Missing` but the queue no longer
        // references it, so it is never re-fetched and the reader hangs (the
        // `player_worker_hls_then_unavailable_mp3_then_mp3_recovery` deadlock).
        let mut deferred: Vec<PlannedFetch> = Vec::new();
        let mut remaining = budget;
        let prefetch_base = self.get_position().max(self.prefetch_anchor());
        let prefetch_byte_cap = ctx
            .look_ahead_bytes
            .map(|n| prefetch_base.saturating_add(n));
        while remaining > 0 {
            hang_tick!();
            let planned = {
                let mut queue = self.queue.lock();
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
                    // Only a present init is ever enqueued (the `rebuild`
                    // gate skips a `None` init), so a missing slot here is
                    // unreachable; skip defensively rather than claim a slot
                    let Some(init) = self.store.init() else {
                        continue;
                    };
                    let Some(handle) = init
                        .state()
                        .try_claim(PlannedFetch::Init, Arc::downgrade(self))
                    else {
                        if !init.state().is_loaded() && !init.state().is_failed() {
                            deferred.push(planned);
                        }
                        continue;
                    };
                    if let Some(actual) = self.store.init_committed_final_len() {
                        handle.into_loaded(actual);
                        ctx.signal.fire();
                        continue;
                    }
                    let Some(cmd) = self.build_init_cmd(ctx, handle) else {
                        if self
                            .store
                            .init()
                            .is_some_and(|i| !i.state().is_loaded() && !i.state().is_failed())
                        {
                            deferred.push(planned);
                        }
                        continue;
                    };
                    out.push(cmd);
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.store.segments().get(seg_idx as usize) else {
                        continue;
                    };
                    let Some(handle) = entry
                        .state()
                        .try_claim(PlannedFetch::Segment(seg_idx), Arc::downgrade(self))
                    else {
                        // Claim failed — another claim owns the slot. Re-queue
                        // unless terminal (`Loaded` = already fetched, `Failed`
                        // = gave up): a `Downloading` orphan settles back to
                        // `Missing` and must stay re-claimable from the queue.
                        if !entry.state().is_loaded() && !entry.state().is_failed() {
                            deferred.push(planned);
                        }
                        continue;
                    };
                    if let Some(actual) = self.store.committed_final_len(seg_idx) {
                        handle.into_loaded(actual);
                        ctx.signal.fire();
                        continue;
                    }
                    let Some(cmd) = self.emit_fetch_cmd(ctx, seg_idx, handle) else {
                        // Acquire raced (the claim was reverted to `Missing`
                        // inside `emit_fetch_cmd`): re-queue, don't drop it.
                        deferred.push(planned);
                        continue;
                    };
                    out.push(cmd);
                }
            }
            remaining -= 1;
        }
        if !deferred.is_empty() {
            let mut queue = self.queue.lock();
            for planned in deferred.into_iter().rev() {
                queue.push_front(planned);
            }
        }
        out
    }

    /// Index of the first non-`Loaded` segment — interpreted as the
    /// "download head" by the ABR controller. Returns `num_segments()`
    /// when every segment is `Loaded`. Scans linearly; cheap because it
    /// only runs from `Abr::progress` (ABR tick cadence).
    pub(crate) fn download_head(&self) -> u32 {
        self.store.download_head()
    }

    #[kithara::probe(
        seek_epoch = ctx.seek_epoch,
        segment_index = u64::from(seg_idx),
        variant = self.variant as u64
    )]
    fn emit_fetch_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        seg_idx: u32,
        handle: FetchClaim<Downloading>,
    ) -> Option<FetchCmd> {
        let entry = &self.store.segments()[seg_idx as usize];
        let Some(resource_handle) = self.store.segment_handle(seg_idx) else {
            let _ = handle.into_missing();
            return None;
        };
        let resource = match resource_handle.acquire(entry.content()) {
            Ok(r) => r,
            Err(err) => {
                debug!(
                    variant = self.variant,
                    seg_idx,
                    error = %err,
                    "emit_fetch_cmd: acquire_resource dropped (variant switch in flight)"
                );
                let _ = handle.into_missing();
                return None;
            }
        };
        self.build_cmd(
            resource_handle.url().clone(),
            resource,
            handle,
            ctx.signal.clone(),
        )
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
            .find_at_offset(byte_offset, self.store.segments())
            .map_or(u64::MAX, |(i, _, _)| u64::from(i))
    )]
    pub(crate) fn find_at_offset(&self, byte_offset: u64) -> Option<(u32, u64, u64)> {
        self.layout
            .find_at_offset(byte_offset, self.store.segments())
    }

    /// Header byte range for format-change resync — alias for
    /// [`Self::header_byte_range`] under the `Source` trait's name.
    pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.header_byte_range()
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
        let store = SegmentStore::new(ctx.scope.clone(), init, segments);
        Arc::new(Self {
            variant,
            store,
            playlist_state,
            layout,
            codec,
            container,
            reader: ReaderRuntime::new(seek_obs),
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

    /// Whether this variant declares a separately fetched `#EXT-X-MAP` init,
    /// regardless of whether its size is yet known.
    pub(crate) fn has_init(&self) -> bool {
        self.store.has_init()
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
        if self.store.init().is_some() {
            return Ok(self.init_byte_range());
        }
        let (_, off, size) = self
            .layout
            .find_natural(0, self.store.segments())
            .ok_or_else(|| {
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
    /// post-commit inits at the `ByteMap` boundary.
    #[kithara::probe(variant = self.variant as u64, size = self.init_size())]
    pub(crate) fn init_byte_range(&self) -> Range<u64> {
        0..self.init_size()
    }

    /// Addressable init prefix range for the byte at `byte_offset`. Returns
    /// `None` when the byte falls outside this variant's *virtually
    /// addressable* init range — the init only counts when
    /// `served_from() == 0` (post-commit, init is orphaned in natural
    /// space). Reads/`contains` for that range route through the init
    /// [`Segment`] (`SegmentStore::init_read_at` / `init_contains`).
    pub(crate) fn init_descriptor_at(&self, byte_offset: u64) -> Option<Range<u64>> {
        if self.served_from() != 0 || !self.store.has_init() {
            return None;
        }
        let range = self.init_byte_range();
        if range.is_empty() || !range.contains(&byte_offset) {
            return None;
        }
        Some(range)
    }

    /// Whether the declared init settled terminally (`Failed`).
    pub(crate) fn init_failed(&self) -> bool {
        self.store.init_failed()
    }

    /// Resource key for the variant's init segment — `None` when the
    /// playlist has no `#EXT-X-MAP` (raw TS/AAC). Test-only assertion helper;
    /// the reader paths read the init through [`SegmentStore::init_handle`].
    #[cfg(test)]
    pub(crate) fn init_resource(&self) -> Option<ResourceKey> {
        self.store.init_resource()
    }

    pub(crate) fn init_size(&self) -> u64 {
        self.store.init_size()
    }

    pub(crate) fn invalidate_init(&self) {
        if self.store.invalidate_init() {
            self.layout.clear_init_seed();
        }
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
    /// Whether the next dispatch should issue the separate init fetch
    /// (CMAF `EXT-X-MAP`) — true only if the variant advertises a
    /// non-zero init segment that hasn't been loaded yet.
    fn needs_init_fetch(&self) -> bool {
        self.store.needs_init_fetch()
    }

    #[must_use]
    pub(crate) fn num_segments(&self) -> u32 {
        self.store.num_segments()
    }

    /// Returns evicted `seg_idx` (`-1` for init), or `None` if `key` doesn't belong to this variant.
    /// State flips `Loaded -> Missing`; queue reseeding is the caller's job
    /// (see `HlsCoord::broadcast_eviction` → `rebuild` for the active
    /// variant; non-active variants' queues are rebuilt lazily on the
    /// next ABR flip).
    #[kithara::probe(variant = self.variant as u64)]
    pub(crate) fn on_evict(&self, key: &ResourceKey) -> Option<i32> {
        self.store.on_evict(key)
    }

    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        // EOF wins over `range_ready`'s zero-width "ready" at `range.start ==
        // total` so the phase is observable as `Eof` at the stream end (see
        // `wait_range`); a flush in flight takes precedence.
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() && !self.reader.is_flushing()
        {
            return SourcePhase::Eof;
        }
        if self.range_ready(&range) {
            return SourcePhase::Ready;
        }
        if self.reader.is_flushing() {
            return SourcePhase::Seeking;
        }
        self.range_wait_phase(&range)
    }

    pub(crate) fn prefetch_anchor(&self) -> u64 {
        self.prefetch_anchor.load(Ordering::Acquire)
    }

    /// Whether any init/media segment covering `range` settled terminally
    /// (`Failed`): the downloader exhausted its retry budget, so the range
    /// will never load. [`wait_range`](Self::wait_range) consults this when
    /// a range is not ready to tell "still downloading" (spin) from
    /// "permanently failed" (terminal error). Walks the same descriptors as
    /// [`range_ready`](Self::range_ready), checking slot state rather than
    /// on-disk bytes; the per-byte `contains_range` walk stays out so this
    /// only fires on a real terminal settle, never on a transient gap.
    fn range_has_failed(&self, range: &Range<u64>) -> bool {
        let total = self.total_bytes();
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        let mut cursor = range.start;
        // The init prefix is not a media segment, so `find_at_offset` returns
        // `None` for a byte inside it — skip past it (jumping to media space)
        // after checking the init's own terminal state, exactly as
        // `range_ready` walks init then media.
        if let Some(init_range) = self.init_descriptor_at(cursor) {
            if self.store.init_failed() {
                return true;
            }
            cursor = init_range.end;
        }
        while cursor < end {
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                break;
            };
            if self.store.segment_failed(seg_idx) {
                return true;
            }
            cursor = (seg_off + seg_size).max(cursor + 1);
        }
        false
    }

    pub(crate) fn range_ready(&self, range: &Range<u64>) -> bool {
        let total = self.total_bytes();
        // When a served segment's size is still unknown, `total` is a lower
        // bound, not the stream end. An offset at/past it is NOT "ready"
        // (clamping `end` to the under-count would falsely report a zero-width
        // ready range and let the reader spin past a real, not-yet-sized
        // segment) — treat it as not-ready so the gate holds Waiting.
        if total > 0 && range.start >= total && !self.sizes_complete() {
            return false;
        }
        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return true;
        }

        let mut cursor = range.start;
        while let Some(init_range) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.store.init_contains(local_start..local_end) {
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
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                return false;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self.store.segment_contains(seg_idx, local_start..local_end) {
                return false;
            }
            cursor = slice_end;
        }
        cursor >= end
    }

    fn range_wait_phase(&self, range: &Range<u64>) -> SourcePhase {
        let total = self.total_bytes();
        if total > 0 && range.start >= total && !self.sizes_complete() {
            let head = self.store.download_head();
            return if self.store.segment_downloading(head) {
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
        }

        let end = if total > 0 {
            range.end.min(total)
        } else {
            range.end
        };
        if range.start >= end {
            return SourcePhase::Waiting;
        }

        let mut waiting_on_demand = false;
        let mut cursor = range.start;
        while let Some(init_range) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.store.init_contains(local_start..local_end) {
                if !self.store.init_downloading() {
                    return SourcePhase::Waiting;
                }
                waiting_on_demand = true;
            }
            cursor = slice_end;
            if cursor >= end {
                return if waiting_on_demand {
                    SourcePhase::WaitingDemand
                } else {
                    SourcePhase::Waiting
                };
            }
        }

        while cursor < end {
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                return SourcePhase::Waiting;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            if !self.store.segment_contains(seg_idx, local_start..local_end) {
                if !self.store.segment_downloading(seg_idx) {
                    return SourcePhase::Waiting;
                }
                waiting_on_demand = true;
            }
            cursor = slice_end;
        }

        if waiting_on_demand {
            SourcePhase::WaitingDemand
        } else {
            SourcePhase::Waiting
        }
    }

    #[kithara::hang_watchdog]
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let total = self.total_bytes();
        if total > 0 && offset >= total && self.sizes_complete() {
            return Ok(ReadOutcome::Eof);
        }

        let buf_len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        let mut written: usize = 0;
        let mut cursor = offset;
        let read_end = offset.saturating_add(buf_len);

        while let Some(init_range) = self.init_descriptor_at(cursor) {
            hang_tick!();
            if cursor >= init_range.end {
                break;
            }
            let slice_end = read_end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self.store.init_read_at(local_start..local_end, dst)? {
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

        // An `#EXT-X-MAP` init occupies the virtual prefix `[0, init_size)`.
        // While the init is declared (`has_init`) but not yet sized
        // (`init_size() == 0` — a failed/absent init HEAD, or the window before
        // the init commits), the offset table transiently seeds segment 0 at
        // offset 0. Serving media here would hand the demuxer segment 0's
        // container where the init's `ftyp`/`moov` belongs
        // ("re_mp4: ftyp not found"), or wedge the reader. Hold the read
        // pending: `needs_init_fetch` keeps the init enqueued and its commit
        // sizes the prefix, after which `init_descriptor_at` routes offset 0
        // to the init. Only the fresh-activation frame (`served_from() == 0`)
        // places the init at offset 0; a switched-in variant's init is
        // orphaned in natural space (see `init_descriptor_at`), so its reads
        // continue past offset 0 and must not be gated here. A terminally
        // failed init (`init_failed`) stops reserving the prefix so the read
        // surfaces an error instead of waiting forever.
        if self.has_init()
            && self.init_size() == 0
            && self.served_from() == 0
            && !self.init_failed()
        {
            return Ok(Self::wrap(written));
        }

        while cursor < read_end {
            hang_tick!();
            let Some((seg_idx, seg_off, seg_size)) = self.find_at_offset(cursor) else {
                break;
            };
            let seg_end = seg_off + seg_size;
            let slice_end = read_end.min(seg_end);
            let local_start = cursor - seg_off;
            let local_end = slice_end - seg_off;
            let take = usize::try_from(local_end - local_start).unwrap_or(usize::MAX);
            let dst = &mut buf[written..written + take];
            match self
                .store
                .segment_read_at(seg_idx, local_start..local_end, dst)?
            {
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
        self.layout.reset(self.init_size(), self.store.segments());
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
        self.store
            .index_at_time(t)
            .and_then(|idx| u32::try_from(idx).ok())
    }

    pub(crate) fn served_from(&self) -> u32 {
        self.layout.served_from()
    }

    #[kithara::probe(variant = self.variant as u64, pos)]
    pub(crate) fn set_position(&self, pos: u64) {
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
            .set_served_until(until, self.store.segments(), self.init_size());
    }

    /// Whether every served segment's byte size is known. While `false`,
    /// [`Self::total_bytes`] is a lower bound (a segment's size estimate is
    /// missing), so the byte-EOF gates must hold `Waiting`/`Pending` rather
    /// than mint EOF for an in-range offset that only looks past-the-end
    /// against the under-count.
    pub(crate) fn sizes_complete(&self) -> bool {
        self.layout.sizes_complete()
    }

    #[kithara::probe(
        variant = self.variant as u64,
        total = self.layout.total_bytes()
    )]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.layout.total_bytes()
    }

    #[kithara::hang_watchdog]
    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        // EOF must win over `range_ready`'s zero-width "ready" at the stream
        // end: a read at `range.start == total` clamps to a `[total, total)`
        // range that `range_ready` reports ready, so the gate would mint
        // `Ready`, `read_at` then returns `Eof`, and the consumer's
        // `phase()` stays `Ready` forever — EOF never becomes observable and a
        // reader polling on phase spins. A seek in flight may pull the
        // position back into the stream, so let the flush path win first.
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() && !self.reader.is_flushing()
        {
            return Ok(WaitOutcome::Eof);
        }
        if self.range_ready(&range) {
            hang_reset!();
            return Ok(WaitOutcome::Ready);
        }
        if self.reader.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }
        // A segment covering this range settled terminally (the downloader
        // exhausted its retries): the bytes will never arrive, so surface a
        // terminal error instead of `WaitBudgetExceeded` — the reader stops
        // here rather than spinning. Checked AFTER flushing/EOF: a seek in
        // flight may be moving the read position off the failed range, and
        // the failed check is scoped to the requested range so seeking to a
        if self.range_has_failed(&range) {
            return Err(StreamError::Source(HlsError::SegmentUnavailable.into()));
        }
        // Not ready: the reader driver (`Stream::probe_read` / `read` /
        // `prime_seek_range`) wakes the peer for this range, per its own
        // on-core/off-core context — this method stays wake-free.
        Err(StreamError::Source(HlsError::WaitBudgetExceeded.into()))
    }

    fn wrap(written: usize) -> ReadOutcome {
        NonZeroUsize::new(written).map_or(
            ReadOutcome::Pending(PendingReason::Retry),
            ReadOutcome::Bytes,
        )
    }
}
