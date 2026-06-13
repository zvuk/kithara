#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    io::Error as IoError,
    marker::PhantomData,
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use kithara_assets::{
    AcquisitionResult, AssetReader, AssetResource, AssetScope, AssetWriter, RawWriteHandle,
    ReadSide, ResourceKey, WriteSide,
};
use kithara_drm::DecryptContext;
use kithara_net::{Headers, NetError, Retryability};
use kithara_platform::{CancelToken, sync::Mutex, time::Duration};
use kithara_storage::{ResourceStatus, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, PendingReason, ReadOutcome, SeekObserve,
    SegmentDescriptor, SourceError, SourcePhase, SourceSeekAnchor, StreamError, StreamResult,
    dl::{FetchCmd, OnCompleteFn, WriterFn},
};
use kithara_test_utils::kithara;
use tracing::{debug, error, warn};
use url::Url;

use crate::{
    HlsError,
    playlist::{PlaylistAccess, PlaylistState},
};

mod cancel_epoch;
mod layout;
mod reader_runtime;
mod segment_store;
use cancel_epoch::CancelEpoch;
use layout::{ActivateParams, Layout};
use reader_runtime::ReaderRuntime;
use segment_store::SegmentStore;

#[cfg(test)]
mod tests;

/// Lock-free four-valued cache-state discriminant for a segment / init
/// slot. `Downloading` exists to dedupe in-flight fetches: `dispatch`
/// only claims (`Missing -> Downloading`) slots before emitting a
/// `FetchCmd`. The settle path drives `Downloading -> Loaded` (success or
/// "another writer already committed"), `Downloading -> Missing`
/// (recoverable failure / cancel), and `Downloading -> Failed` (terminal:
/// the downloader exhausted its retry budget). Eviction is the only
/// producer of `Loaded -> Missing`.
///
/// `Failed` is terminal by construction: `try_claim` only CAS's from
/// `Missing`, so a failed slot is never re-dispatched (no extra scheduler
/// check needed) and a reader observing it via `is_failed` surfaces a
/// terminal error instead of spinning.
///
/// The bit values are private and the only mutators are the typed
/// transitions on the phase-specific `impl Segment<Downloading>` /
/// `impl Segment<Loaded>` blocks, so there is no silent `From<u8>`
/// fallback. Reads stay a plain atomic (no lock) because `download_head`
/// scans every slot on the ABR tick.
#[derive(Debug)]
struct SegmentSlotState(AtomicU8);

impl SegmentSlotState {
    const MISSING: u8 = 0;
    const DOWNLOADING: u8 = 1;
    const LOADED: u8 = 2;
    const FAILED: u8 = 3;

    fn missing() -> Arc<Self> {
        Arc::new(Self(AtomicU8::new(Self::MISSING)))
    }

    fn is_loaded(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::LOADED
    }

    /// Terminal-failure probe. A `Failed` slot will never load (the
    /// downloader gave up); readers surface a terminal error on it.
    fn is_failed(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::FAILED
    }

    /// Atomic `Missing -> Downloading` claim. Returns the owned
    /// [`Segment<Downloading>`](Segment) handle when the caller now owns
    /// the in-flight slot, `None` when another caller already claimed it.
    fn try_claim(
        self: &Arc<Self>,
        planned: PlannedFetch,
        variant: Weak<HlsVariant>,
    ) -> Option<Segment<Downloading>> {
        self.0
            .compare_exchange(
                Self::MISSING,
                Self::DOWNLOADING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| Segment {
                data: DownloadClaim {
                    slot: Arc::clone(self),
                    planned,
                    variant,
                    settled: false,
                },
                _phase: PhantomData,
            })
    }

    fn mark_loaded(&self) {
        self.0.store(Self::LOADED, Ordering::Release);
    }

    fn mark_missing(&self) {
        self.0.store(Self::MISSING, Ordering::Release);
    }

    fn mark_failed(&self) {
        self.0.store(Self::FAILED, Ordering::Release);
    }
}

/// Whether a fetch error is terminal for the slot. The net layer's
/// resilient body already retried stalls and transient body errors, so a
/// `Fatal` error reaching the settle path means the downloader gave up —
/// the slot must be parked `Failed` rather than re-dispatched. `Cancelled`
/// is the one `Fatal` variant that stays recoverable: a cancel marks an
/// epoch rebuild, which owns the re-dispatch, so it keeps `Missing`.
fn is_terminal_fetch_error(e: &NetError) -> bool {
    !matches!(e, NetError::Cancelled) && e.retryability() == Retryability::Fatal
}

mod sealed {
    pub(super) trait Sealed {}
}

/// Compile-time download phase of a segment / init slot. The phantom
/// parameter on [`Segment`] encodes which transitions are legal, so the
/// invariants that `SegmentSlotState` used to check at runtime become
/// type errors: only a `Segment<Downloading>` can settle, and it settles
/// by consuming itself into a `Segment<Loaded>` or `Segment<Missing>`.
///
/// Sealed — the phase set is closed to this module. Each phase carries its
/// own [`Data`](SegmentPhase::Data) payload; phases without state use `()`.
trait SegmentPhase: sealed::Sealed {
    type Data;
}

/// In-flight: claimed via a `Missing -> Downloading` CAS, fetch pending.
struct Downloading;
/// Committed on disk; carries the resolved `final_len`.
struct Loaded;
/// Returned to the dispatch pool (recoverable failure / cancel / evict).
struct Missing;
/// Terminal: the downloader exhausted its retry budget on this slot. Never
/// re-dispatched (`try_claim` only CAS's from `Missing`) and surfaced to
/// readers as a terminal error via [`SegmentSlotState::is_failed`].
struct Failed;

impl sealed::Sealed for Downloading {}
impl sealed::Sealed for Loaded {}
impl sealed::Sealed for Missing {}
impl sealed::Sealed for Failed {}

impl SegmentPhase for Downloading {
    type Data = DownloadClaim;
}
impl SegmentPhase for Loaded {
    type Data = LoadedProof;
}
impl SegmentPhase for Missing {
    type Data = ();
}
impl SegmentPhase for Failed {
    type Data = ();
}

/// Phantom-typed handle to a segment / init slot. `S` is one of
/// [`Downloading`], [`Loaded`], [`Missing`]; the per-phase fields live in
/// `S::Data`. Transitions are consume-self methods on the phase-specific
/// `impl` blocks below, so the compiler rejects a double settle or an
/// [`apply_commit`](HlsVariant::apply_commit) on anything but a `Loaded`
/// handle.
struct Segment<S: SegmentPhase> {
    data: S::Data,
    _phase: PhantomData<S>,
}

/// Backing payload of a [`Segment<Downloading>`](Segment). Shares the slot
/// CAS cell so a terminal transition can flip it, holds the `Weak`
/// back-reference for the post-commit size apply, and carries the `Drop`
/// disarm flag.
struct DownloadClaim {
    slot: Arc<SegmentSlotState>,
    planned: PlannedFetch,
    variant: Weak<HlsVariant>,
    settled: bool,
}

/// Backing payload of a [`Segment<Loaded>`](Segment): the committed slot
/// identity and resolved size consumed by [`HlsVariant::apply_commit`].
struct LoadedProof {
    planned: PlannedFetch,
    final_len: u64,
}

impl Segment<Downloading> {
    /// `Downloading -> Loaded` with a post-commit size apply. `actual` is
    /// the on-disk `final_len` (success / cache-hit / committed-by-race).
    /// `apply_commit` shrinks the variant's layout to match *before*
    /// `mark_loaded` flips the slot — a reader that observes `Loaded` then
    /// reads the size must never see the stale estimate.
    fn into_loaded(mut self, actual: u64) -> Segment<Loaded> {
        let loaded = Segment {
            data: LoadedProof {
                planned: self.data.planned,
                final_len: actual,
            },
            _phase: PhantomData,
        };
        if let Some(v) = self.data.variant.upgrade() {
            v.apply_commit(&loaded);
        }
        self.data.slot.mark_loaded();
        self.data.settled = true;
        loaded
    }

    /// `Downloading -> Loaded` without a size apply — the resource
    /// committed by a racing writer but reported no `final_len`, so the
    /// existing layout estimate stands.
    fn into_loaded_no_apply(mut self) -> Segment<Loaded> {
        self.data.slot.mark_loaded();
        self.data.settled = true;
        Segment {
            data: LoadedProof {
                planned: self.data.planned,
                final_len: 0,
            },
            _phase: PhantomData,
        }
    }

    /// `Downloading -> Missing` recovery (recoverable failure / cancel
    /// before commit). The slot returns to the dispatch pool.
    fn into_missing(mut self) -> Segment<Missing> {
        self.data.slot.mark_missing();
        self.data.settled = true;
        Segment {
            data: (),
            _phase: PhantomData,
        }
    }

    /// `Downloading -> Failed` terminal settle: the downloader exhausted
    /// its retry budget (the net layer's resilient body already retried
    /// the stall/transient errors), so the slot is parked permanently —
    /// `try_claim` will not re-dispatch it and a waiting reader surfaces a
    /// terminal error. Unlike [`into_missing`](Self::into_missing) the
    /// slot does NOT return to the dispatch pool.
    fn into_failed(mut self) -> Segment<Failed> {
        self.data.slot.mark_failed();
        self.data.settled = true;
        Segment {
            data: (),
            _phase: PhantomData,
        }
    }

    /// Consume the claim without touching slot state — used for a stale
    /// (cancelled) settle whose resource already committed: the new epoch
    /// owns the slot, so leaving it as-is is correct.
    fn abandon(mut self) {
        self.data.settled = true;
    }
}

impl Segment<Loaded> {
    fn planned(&self) -> PlannedFetch {
        self.data.planned
    }

    fn final_len(&self) -> u64 {
        self.data.final_len
    }
}

/// The `Drop` safety net lives on the concrete payload (not on the generic
/// `Segment<Downloading>`, which `Drop` cannot specialize): if a claim is
/// dropped without a transition, the slot reverts to `Missing` so a leaked
/// handle can never strand it in `Downloading`. The consume-self
/// transitions set `settled` first, disarming this no-op.
impl Drop for DownloadClaim {
    fn drop(&mut self) {
        if !self.settled {
            self.slot.mark_missing();
            warn!(
                target: "kithara_hls::settle",
                planned = ?self.planned,
                "Downloading claim dropped without settle — slot reverted to Missing"
            );
        }
    }
}

/// Decryption disposition for a segment / init resource. Replaces
/// `decrypt_ctx: Option<DecryptContext>`, whose `.is_some()` /
/// `.map_or_else` discrimination on the acquire path conflated "no key"
/// with "cleartext". `Plain` is the explicit cleartext case; `Encrypted`
/// carries the AES-128 [`DecryptContext`].
#[derive(Debug, Clone)]
enum SegmentContent {
    Plain,
    Encrypted(DecryptContext),
}

impl From<Option<DecryptContext>> for SegmentContent {
    fn from(ctx: Option<DecryptContext>) -> Self {
        ctx.map_or(Self::Plain, Self::Encrypted)
    }
}

#[derive(Debug)]
pub(crate) struct SegmentEntry {
    /// Cache state. The owning [`Segment<Downloading>`](Segment) handle (held by the
    /// segment's `FetchSlot`) shares this `Arc` and flips it on settle:
    /// `Loaded` on success, `Missing` on recoverable failure. Stale
    /// settles (cancelled before completion) are gated by
    /// `FetchSlot.cancel` and leave the slot untouched.
    state: Arc<SegmentSlotState>,
    /// Encrypted media size from HEAD, shrunk to the actual post-decrypt
    /// length by [`HlsVariant::apply_commit`] when `commit` reports
    /// the resource's `final_len`. Reader queries
    /// (`segment_size`, `total_bytes`) read through this atomic.
    size: AtomicU64,
    decode_time: Duration,
    duration: Duration,
    content: SegmentContent,
    resource_id: ResourceKey,
    url: Url,
}

#[derive(Debug)]
pub(crate) struct InitEntry {
    /// Shared with the init segment's `FetchSlot` handle; see
    /// [`SegmentEntry::state`].
    state: Arc<SegmentSlotState>,
    /// Encrypted init size from HEAD, shrunk on commit — see
    /// [`SegmentEntry::size`].
    size: AtomicU64,
    /// Decryption disposition for the init segment. HLS init segments
    /// don't carry their own `#EXT-X-KEY`; an encrypted variant mirrors
    /// the first media segment's key — the standard packaging convention.
    content: SegmentContent,
    resource_id: ResourceKey,
    url: Url,
}

/// Whether a variant carries a *separately fetched* init segment. The
/// discriminant is the construction-time init size, which subsumes all
/// three "no separate init fetch" cases that were previously the dynamic
/// `init_size() == 0` check: no `#EXT-X-MAP`, a byte-range-embedded init
/// (init lives in segment 0's byte range), or a failed init HEAD. The size
/// of a [`Pending`](VariantInit::Pending) init only ever shrinks on commit
/// (HEAD estimate -> committed `final_len`) and never crosses back to 0, so
/// the frozen discriminant stays equivalent to the old runtime probe — see
/// the crate `README.md` "Variant init".
#[derive(Debug)]
pub(crate) enum VariantInit {
    /// No separate init fetch. Carries no resource: `rebuild` never enqueues
    /// `PlannedFetch::Init`, and every init-size query reads as 0.
    NotApplicable,
    /// A real, separately fetched init segment (`#EXT-X-MAP` with a known
    /// size). Always has a real URL.
    Pending(InitEntry),
}

impl VariantInit {
    /// The `Pending` entry, or `None` for [`NotApplicable`](Self::NotApplicable).
    fn pending(&self) -> Option<&InitEntry> {
        match self {
            Self::NotApplicable => None,
            Self::Pending(entry) => Some(entry),
        }
    }

    /// In-memory init size: 0 when [`NotApplicable`](Self::NotApplicable),
    /// else the `Pending` entry's current (possibly post-commit shrunk)
    /// size atom.
    fn size(&self) -> u64 {
        self.pending()
            .map_or(0, |entry| entry.size.load(Ordering::Acquire))
    }

    /// Unwrap the `Pending` entry for assertions on a variant known to
    /// carry a separately fetched init.
    #[cfg(test)]
    fn expect_pending(&self) -> &InitEntry {
        self.pending().expect("init is Pending")
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
    pub(crate) seg_boundary: u64,
    pub(crate) reader_pos: u64,
}

pub(crate) struct HlsVariant {
    /// Segment/init content domain: the asset scope plus the init and
    /// media entry tables. Owns every resource read (`read_resource`,
    /// `contains_range`) — the produce-core's open-and-read live here, and
    /// it is the home for the `WS5d` held-resource lease. The fetch path on
    /// this facade borrows its `init()` / `segments()` to claim slots and
    /// build `FetchCmd`s.
    store: SegmentStore,
    /// Parsed master/media-playlist data. Owned by `Arc` so multiple
    /// variants share a single immutable view; used by
    /// `seek_time_anchor` (`find_seek_point_for_time`) and as the
    /// source for the cached [`Self::codec`] / [`Self::container`].
    playlist_state: Arc<PlaylistState>,
    /// Reader-side runtime: the shared byte cursor, peer wake handle, and the
    /// timeline consulted for `is_flushing` gating. The probe-tagged
    /// `get_position` / `set_position` stay on the facade and delegate here.
    reader: ReaderRuntime,
    /// Coherent owner of the cross-variant byte-address-space coordinates
    /// (`byte_shift`, `served_from`, `served_until`, `init_seed`, the media
    /// offset table). A single lock guards all five so a reader never mixes
    /// the shift of one activation with the served bounds of the next.
    layout: Layout,
    prefetch_anchor: AtomicU64,
    /// The variant's cancel epoch: the per-track parent (mirror of
    /// `coord.cancel` = `PlanCtx::master_cancel`) plus the rotating
    /// per-activation child. Survives variant re-activation — a cross-codec
    /// `commit_variant_switch` may flip from `v_old` to `v_new` and back to
    /// `v_old`, and the second activation of `v_old` must dispatch fetches
    /// under a *live* cancel, so [`Self::rearm_cancel`] rotates the child.
    cancel_epoch: CancelEpoch,
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
    variant: usize,
}

/// Media payload + shared-dep snapshot for [`HlsVariant::from_parts`].
/// Pulled out of the constructor's argument list so the call site stays
/// readable — `new()` builds this from parsed playlist metadata, tests
/// build it inline from synthesised fixtures.
pub(crate) struct VariantParts {
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) init: VariantInit,
    pub(crate) codec: Option<AudioCodec>,
    pub(crate) container: Option<ContainerFormat>,
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
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
        seek_obs: Arc<dyn SeekObserve>,
        init_decrypt_ctx: Option<DecryptContext>,
        decrypt_contexts: &[Option<DecryptContext>],
        ctx: &PlanCtx,
    ) -> Arc<Self> {
        let init = Self::build_init_entry(
            playlist_state.as_ref(),
            variant,
            init_decrypt_ctx,
            &ctx.scope,
        );
        let segments = Self::build_segment_entries(
            playlist_state.as_ref(),
            decrypt_contexts,
            variant,
            init.size(),
            &ctx.scope,
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
                seek_obs,
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
    fn apply_commit(&self, loaded: &Segment<Loaded>) {
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
        handle: Segment<Downloading>,
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
        };
        Some(
            FetchCmd::get(url)
                .cancel(cancel)
                .maybe_headers(self.headers.clone())
                .writer(slot.writer())
                .on_complete(OnCompleteFn::from(slot))
                .build(),
        )
    }

    fn build_init_cmd(
        self: &Arc<Self>,
        ctx: &PlanCtx,
        handle: Segment<Downloading>,
    ) -> Option<FetchCmd> {
        let init = self.store.init().pending()?;
        let resource = match &init.content {
            SegmentContent::Plain => ctx
                .scope
                .store()
                .acquire_resource(&init.resource_id, None)
                .expect("acquire_resource for init must succeed"),
            SegmentContent::Encrypted(c) => ctx
                .scope
                .store()
                .acquire_resource_with_ctx(&init.resource_id, None, Some(c.clone()))
                .expect("acquire_resource_with_ctx for init must succeed"),
        };
        self.build_cmd(init.url.clone(), resource, handle)
    }

    fn build_init_entry(
        playlist_state: &PlaylistState,
        variant_idx: usize,
        decrypt_ctx: Option<DecryptContext>,
        scope: &AssetScope<DecryptContext>,
    ) -> VariantInit {
        let size = playlist_state.init_size(variant_idx);
        // A sized init always carries an `#EXT-X-MAP` URL (size is only
        // populated from a successful init HEAD, which requires the URL),
        // so `Pending` always has a real URL.
        let Some(url) = (size > 0)
            .then(|| playlist_state.init_url(variant_idx))
            .flatten()
        else {
            return VariantInit::NotApplicable;
        };
        VariantInit::Pending(InitEntry {
            resource_id: scope.key_from_url(&url),
            url,
            state: SegmentSlotState::missing(),
            size: AtomicU64::new(size),
            content: SegmentContent::from(decrypt_ctx),
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
        scope: &AssetScope<DecryptContext>,
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
                resource_id: scope.key_from_url(&url),
                url,
                state: SegmentSlotState::missing(),
                size: AtomicU64::new(media_size),
                content: SegmentContent::from(decrypt_ctx),
                decode_time,
                duration,
            });
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
        let entry = self.store.segments().get(idx)?;
        let seg_idx_u32 = u32::try_from(idx).ok()?;
        let byte_offset = self.layout.natural_offset(idx)?;
        let size = self.store.segment_size(idx)?;
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
        let (idx, _, _) = self.find_at_offset(byte)?;
        self.descriptor(idx as usize)
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
                    // Only a `Pending` init is ever enqueued (the `rebuild`
                    // gate skips `NotApplicable`), so a missing entry here is
                    // unreachable; skip defensively rather than claim a slot
                    // that does not exist.
                    let Some(init) = self.store.init().pending() else {
                        continue;
                    };
                    let Some(handle) = init
                        .state
                        .try_claim(PlannedFetch::Init, Arc::downgrade(self))
                    else {
                        continue;
                    };
                    if let Some(actual) = self.store.committed_final_len(&init.resource_id) {
                        handle.into_loaded(actual);
                        continue;
                    }
                    out.extend(self.build_init_cmd(ctx, handle));
                }
                PlannedFetch::Segment(seg_idx) => {
                    let Some(entry) = self.store.segments().get(seg_idx as usize) else {
                        continue;
                    };
                    let Some(handle) = entry
                        .state
                        .try_claim(PlannedFetch::Segment(seg_idx), Arc::downgrade(self))
                    else {
                        continue;
                    };
                    if let Some(actual) = self.store.committed_final_len(&entry.resource_id) {
                        handle.into_loaded(actual);
                        continue;
                    }
                    let Some(cmd) = self.emit_fetch_cmd(ctx, seg_idx, handle) else {
                        continue;
                    };
                    out.push(cmd);
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
        handle: Segment<Downloading>,
    ) -> Option<FetchCmd> {
        let entry = &self.store.segments()[seg_idx as usize];
        let acquire = match &entry.content {
            SegmentContent::Plain => ctx.scope.store().acquire_resource(&entry.resource_id, None),
            SegmentContent::Encrypted(c) => ctx.scope.store().acquire_resource_with_ctx(
                &entry.resource_id,
                None,
                Some(c.clone()),
            ),
        };
        let resource = match acquire {
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
        self.build_cmd(entry.url.clone(), resource, handle)
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
        let init_size = init.size();
        let layout = Layout::new(init_size, &segments);
        let store = SegmentStore::new(ctx.scope.clone(), init, segments);
        Arc::new(Self {
            variant,
            store,
            playlist_state,
            reader: ReaderRuntime::new(seek_obs),
            layout,
            codec,
            container,
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

    /// Byte range a demuxer reads to re-establish container state after a
    /// format change (variant flip or codec change).
    ///
    /// `Ok(init_range)` for `served_from() == 0`, else
    /// `Err(FormatChangeNotApplicable)` for byte-shifted same-codec
    /// commits. See the crate `README.md` "Format-change header byte range".
    pub(crate) fn header_byte_range(&self) -> StreamResult<Range<u64>> {
        if self.served_from() != 0 {
            return Err(StreamError::Source(SourceError::FormatChangeNotApplicable));
        }
        if self.store.init().pending().is_some() {
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
        if self.range_ready(&range) {
            return SourcePhase::Ready;
        }
        if self.reader.is_flushing() {
            return SourcePhase::Seeking;
        }
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    pub(crate) fn prefetch_anchor(&self) -> u64 {
        self.prefetch_anchor.load(Ordering::Acquire)
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
        while let Some((ref key, init_range)) = self.init_descriptor_at(cursor) {
            if cursor >= init_range.end {
                break;
            }
            let slice_end = end.min(init_range.end);
            let local_start = cursor - init_range.start;
            let local_end = slice_end - init_range.start;
            if !self.store.contains_range(key, local_start..local_end) {
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
            if !self.store.contains_range(key, local_start..local_end) {
                return false;
            }
            cursor = slice_end;
        }
        cursor >= end
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
        if let Some((_key, init_range)) = self.init_descriptor_at(cursor) {
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
            match self.store.read_resource(key, local_start..local_end, dst)? {
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
            match self
                .store
                .read_resource(&key, local_start..local_end, dst)?
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
    /// header to construct the codec. See the crate `README.md`
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

    pub(crate) fn segment_resource(&self, seg_idx: u32) -> Option<ResourceKey> {
        self.store.segment_resource(seg_idx)
    }

    pub(crate) fn served_from(&self) -> u32 {
        self.layout.served_from()
    }

    /// Coherent "is this variant historical?" check — `served_from` and
    /// `served_until` read under a single Layout lock.
    pub(crate) fn is_shrunk(&self) -> bool {
        self.layout.is_shrunk(self.num_segments())
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

    #[kithara::probe(
        variant = self.variant as u64,
        total = self.layout.total_bytes()
    )]
    pub(crate) fn total_bytes(&self) -> u64 {
        self.layout.total_bytes()
    }

    /// Whether every served segment's byte size is known. While `false`,
    /// [`Self::total_bytes`] is a lower bound (a segment's size estimate is
    /// missing), so the byte-EOF gates must hold `Waiting`/`Pending` rather
    /// than mint EOF for an in-range offset that only looks past-the-end
    /// against the under-count.
    pub(crate) fn sizes_complete(&self) -> bool {
        self.layout.sizes_complete()
    }

    #[kithara::hang_watchdog]
    pub(crate) fn wait_range(
        &self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        if self.range_ready(&range) {
            hang_reset!();
            return Ok(WaitOutcome::Ready);
        }
        if self.reader.is_flushing() {
            return Ok(WaitOutcome::Interrupted);
        }
        let total = self.total_bytes();
        if total > 0 && range.start >= total && self.sizes_complete() {
            return Ok(WaitOutcome::Eof);
        }
        // A segment covering this range settled terminally (the downloader
        // exhausted its retries): the bytes will never arrive, so surface a
        // terminal error instead of `WaitBudgetExceeded` — the reader stops
        // here rather than spinning. Checked AFTER flushing/EOF: a seek in
        // flight may be moving the read position off the failed range, and
        // the failed check is scoped to the requested range so seeking to a
        // healthy range still plays.
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
    handle: Segment<Downloading>,
    /// Sole commit owner (non-`Clone`); consumed in `settle`.
    writer: AssetWriter<DecryptContext>,
    /// Read view of the writer's generation — used to observe a
    /// committed-by-race status before deciding the terminal transition.
    reader: AssetReader<DecryptContext>,
    /// Clone-able streaming-write handle for the fetch body closure.
    raw: RawWriteHandle,
    cancel: CancelToken,
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
    ///
    /// Consumes the slot (`OnCompleteFn` is `FnOnce`): the owned
    /// [`Segment<Downloading>`](Segment) handle is moved into exactly one
    /// terminal transition, so the slot state can never be double-driven.
    fn settle(self, bytes_written: u64, err: Option<&NetError>) {
        if self.cancel.is_cancelled() {
            self.settle_cancelled(bytes_written);
            return;
        }
        match err {
            None => self.settle_success(bytes_written),
            Some(e) => self.settle_failure(e),
        }
    }

    fn settle_cancelled(self, bytes_written: u64) {
        let Self {
            handle,
            writer,
            reader,
            ..
        } = self;
        let committed = matches!(reader.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", bytes_written, committed, "stale (cancelled)");
        if committed {
            // Committed by the new epoch's writer — dropping our (stale)
            // writer fails only its own generation's gate; the cleanup is
            // race-safe (skips removal when the live state is Committed).
            drop(writer);
            handle.abandon();
        } else {
            writer.fail("fetch cancelled before completion".into());
            handle.into_missing();
        }
    }

    fn settle_failure(self, e: &NetError) {
        let Self {
            handle,
            writer,
            reader,
            ..
        } = self;
        let committed = matches!(reader.status(), ResourceStatus::Committed { .. });
        debug!(target: "kithara_hls::settle", err = %e, committed, "fail-path");
        if committed {
            // Committed by the new epoch's writer; ours never wrote — drop it
            // (cleanup is race-safe) and adopt the on-disk length.
            drop(writer);
            if let ResourceStatus::Committed { final_len: Some(n) } = reader.status() {
                handle.into_loaded(n);
            } else {
                handle.into_loaded_no_apply();
            }
        } else {
            writer.fail(e.to_string());
            // The net layer's resilient body already retried stalls and
            // transient body errors; a `Fatal` error here (e.g.
            // `RetryExhausted`) means the downloader gave up, so the slot
            // is terminal — parking it as `Failed` stops the re-dispatch
            // loop and lets a waiting reader surface a terminal error.
            // `Cancelled` is recoverable (epoch rebuild owns it) and stays
            // `Missing`. The typed cause is logged here; readers see only
            // the fixed terminal message (no transport detail).
            if is_terminal_fetch_error(e) {
                error!(
                    target: "kithara_hls::settle",
                    err = %e,
                    "terminal fetch failure — slot parked Failed, will not re-dispatch"
                );
                handle.into_failed();
            } else {
                handle.into_missing();
            }
        }
    }

    fn settle_success(self, bytes_written: u64) {
        let Self { handle, writer, .. } = self;
        // Consume-self commit returns the Ready reader; read `final_len` off it
        // (PKCS7 unpad shrinks DRM segments below the HEAD estimate).
        match writer.commit(Some(bytes_written)) {
            Ok(reader) => {
                debug!(target: "kithara_hls::settle", bytes_written, "success");
                let actual = match reader.status() {
                    ResourceStatus::Committed { final_len: Some(n) } => n,
                    _ => bytes_written,
                };
                handle.into_loaded(actual);
            }
            Err(e) => {
                debug!(
                    target: "kithara_hls::settle",
                    bytes_written,
                    err = %e,
                    "success-but-commit-failed"
                );
                handle.into_missing();
            }
        }
    }

    fn writer(&self) -> WriterFn {
        let raw = self.raw.clone();
        let offset = Arc::new(AtomicU64::new(0));
        Box::new(move |chunk: &[u8]| {
            let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            raw.write_at(pos, chunk).map_err(IoError::other)
        })
    }
}
