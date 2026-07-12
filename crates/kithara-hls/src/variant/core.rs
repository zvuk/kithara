use std::{
    collections::VecDeque,
    ops::Deref,
    sync::atomic::{AtomicU32, AtomicU64},
};

use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
    traits::FromWithParams,
};
use kithara_stream::{AudioCodec, ContainerFormat, SeekObserve};

use super::{
    cancel_epoch::CancelEpoch,
    cas_anchor::CasAnchorCell,
    offsets::Layout,
    probe::SizeDemandState,
    reader_runtime::ReaderRuntime,
    seqlock::{AtomicOptU64, AtomicSeekAlias},
};
use crate::{
    config::SizeProbeMethod,
    playlist::{PlaylistAccess, PlaylistState},
    segment::{MediaSegment, PlannedFetch, Segment, SegmentContent, SegmentSize, SegmentSlotState},
    signal::SizeSignal,
};

pub(super) const INIT_PLACEHOLDER_BYTES: u64 = 16 * 1024;

pub(crate) struct PlanCtx {
    pub(crate) bus: EventBus,
    pub(crate) scope: kithara_assets::AssetScope,
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
    /// Max media segments the downloader may keep ahead of the reader.
    /// This is derived from small ephemeral cache capacity, where byte-only
    /// lookahead can otherwise prefetch more resources than the cache can retain
    /// and trigger eviction/rebuild thrash.
    pub(crate) look_ahead_segments: Option<usize>,
    pub(crate) size_probe_method: SizeProbeMethod,
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
    /// Coherent owner of the cross-variant byte-address-space coordinates
    /// (`byte_shift`, `served_from`, `served_until`, `init_seed`, the media
    /// offset table). A single lock guards all five so a reader never mixes
    /// the shift of one activation with the served bounds of the next.
    pub(super) layout: Layout,
    pub(super) flow: VariantFlow,
    pub(super) profile: VariantProfile,
    pub(super) seek: VariantSeek,
    pub(super) segments: VariantSegments,
    pub(super) variant: usize,
}

pub(super) struct VariantProfile {
    /// Cached audio codec — pulled from `playlist_state` at construction
    /// time. The reader's hot path (`media_info`) reads this without
    /// taking the playlist's per-variant `RwLock`.
    pub(super) codec: Option<AudioCodec>,
    /// Cached container format — see [`Self::codec`].
    pub(super) container: Option<ContainerFormat>,
    /// HTTP headers applied to every `FetchCmd` this variant emits.
    /// Snapshotted from `PlanCtx::headers` at construction; carries
    /// resource-wide auth (e.g. zvuk `X-Auth-Token`) so segment GETs
    /// reach the same authenticated endpoint as the playlist load.
    pub(super) headers: Option<Headers>,
    pub(super) bus: EventBus,
}

pub(super) struct VariantFlow {
    pub(super) prefetch_anchor: AtomicU64,
    /// The variant's cancel epoch: the per-track parent (mirror of
    /// `coord.cancel` = `PlanCtx::master_cancel`) plus the rotating
    /// per-activation child. Survives variant re-activation — a cross-codec
    /// `commit_variant_switch` may flip from `v_old` to `v_new` and back to
    /// `v_old`, and the second activation of `v_old` must dispatch fetches
    /// under a *live* cancel, so [`HlsVariant::rearm_cancel`] rotates the child.
    pub(super) cancel_epoch: CancelEpoch,
    pub(super) queue: Mutex<VecDeque<PlannedFetch>>,
    /// Reader-side runtime: the shared byte cursor, peer wake handle, and the
    /// timeline consulted for `is_flushing` gating. The probe-tagged
    /// `get_position` / `set_position` stay on the facade and delegate here.
    pub(super) reader: ReaderRuntime,
}

pub(super) struct VariantSeek {
    /// Lock-free, allocation-free exact-byte-seek demand, read and cleared on
    /// the produce-core metadata-phase gate
    /// ([`HlsVariant::exact_byte_metadata_phase`]). A single `AtomicU64` with a
    /// `u64::MAX` none sentinel.
    pub(super) exact_byte_seek: AtomicOptU64,
    /// Lock-free, allocation-free seek-alias snapshot. The produce-core read
    /// path ([`HlsVariant::seek_alias_at`], reached on every `find_at_offset`)
    /// and the steady-read-path clear (`advance`) touch only atomics; the base
    /// is single-writer (on-core), the resolved exact anchor is published
    /// off-RT under a generation tag. See `flow/seqlock.rs`.
    pub(super) alias: AtomicSeekAlias,
    pub(super) segment_aware_tail: AtomicU32,
    /// Lock-free, allocation-free exact-seek demand, read on the produce-core
    /// metadata-phase gate ([`HlsVariant::exact_seek_metadata_phase`]). Body is
    /// MULTI-writer: the on-core seek path (`seek_time_anchor`) and the off-RT
    /// downloader seek-epoch reset (`rebuild_at_time`) both SET it with no lock
    /// between them, so the cell serializes writers with a CAS-acquired version
    /// and the RT reader bails to not-ready (never spins) on a write-in-flight.
    /// Off-RT completers CAS-consume the generation.
    pub(super) exact_seek: CasAnchorCell,
    pub(super) size_demand: Mutex<SizeDemandState>,
}

pub(super) struct VariantSegments {
    /// Per-track asset store. The single source-of-truth clone: each vended
    /// [`ResourceHandle`] gets a cheap clone of it, and the fetch path
    /// acquires *writable* resources via the same clone in `PlanCtx::scope`.
    /// Vends a narrow `ResourceHandle` per segment/init (`segment_handle` /
    /// `init_handle`); the produce-core's disk read and acquire flow through
    /// that handle, and it is the home for the `WS5d` held-resource lease.
    pub(super) scope: kithara_assets::AssetScope,
    /// Init slot: `Some(Segment::Init)` for a variant that advertises a
    /// separately fetched `#EXT-X-MAP` init, `None` otherwise. Its existence
    /// is keyed on the playlist `#EXT-X-MAP` URL, never on the known byte size
    /// — see [`Self::build_init_entry`].
    pub(super) init: Option<Segment>,
    /// Media entry table — the fetch path and descriptor builders index it
    /// for `url` / `content` / `decode_time`.
    entries: Vec<Segment>,
}

impl Deref for VariantSegments {
    type Target = [Segment];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl VariantSegments {
    fn new(
        scope: kithara_assets::AssetScope,
        init: Option<Segment>,
        entries: Vec<Segment>,
    ) -> Self {
        Self {
            scope,
            init,
            entries,
        }
    }
}

impl VariantFlow {
    fn new(
        master_cancel: CancelToken,
        seek_obs: Arc<dyn SeekObserve>,
        queue_capacity: usize,
    ) -> Self {
        Self {
            cancel_epoch: CancelEpoch::new(master_cancel),
            prefetch_anchor: AtomicU64::new(0),
            // Preallocate to the worst-case rebuild size (init + every media
            // segment + the seg-0 decoder probe) so the per-seek
            // `clear` + `extend` in `rebuild_queue` never reallocates on the
            queue: Mutex::new(VecDeque::with_capacity(queue_capacity)),
            reader: ReaderRuntime::new(seek_obs),
        }
    }
}

impl VariantSeek {
    fn new(no_seek_tail: u32) -> Self {
        Self {
            alias: AtomicSeekAlias::new(),
            exact_seek: CasAnchorCell::new(),
            exact_byte_seek: AtomicOptU64::none(),
            segment_aware_tail: AtomicU32::new(no_seek_tail),
            size_demand: Mutex::default(),
        }
    }
}

/// Media payload + shared-dep snapshot that owns bare variant assembly.
/// Production builds this from parsed playlist metadata; tests build it
/// inline from synthesised fixtures.
pub(crate) struct VariantParts {
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

/// Route-only non-exact media placeholder for playlists without byte ranges.
/// `EXTINF` keeps the virtual byte layout proportional to media time without
/// pretending to know payload length. Exactness gates still prevent
/// authoritative EOF and file-like seek math until body commit or a lazy
/// exact-size demand publishes the real length.
pub(super) fn segment_placeholder_size(duration: Duration, bandwidth_bps: Option<u64>) -> u64 {
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
    pub(crate) ctx: &'a PlanCtx,
    pub(crate) decrypt_contexts: &'a [Option<DecryptContext>],
    pub(crate) seek_obs: Arc<dyn SeekObserve>,
    pub(crate) init_decrypt_ctx: Option<DecryptContext>,
    pub(crate) variant_idx: usize,
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
            ctx,
        );
        let segments = HlsVariant::build_segment_entries(
            playlist_state.as_ref(),
            decrypt_contexts,
            variant_idx,
            ctx,
        );
        let codec = playlist_state.variant_codec(variant_idx);
        let container = playlist_state.variant_container(variant_idx);
        VariantParts {
            seek_obs,
            codec,
            container,
            init,
            segments,
        }
        .into_variant(variant_idx, ctx)
    }
}

impl VariantParts {
    /// Bare assembly used by unit tests inside this module.
    #[must_use]
    pub(crate) fn into_variant(self, variant: usize, ctx: &PlanCtx) -> Arc<HlsVariant> {
        let Self {
            init,
            codec,
            container,
            seek_obs,
            segments,
        } = self;
        let init_size = init_size_of(&init);
        let layout = Layout::new(init_size, &segments);
        Arc::new(HlsVariant {
            variant,
            layout,
            flow: VariantFlow::new(
                ctx.master_cancel.clone(),
                seek_obs,
                segments.len().saturating_add(2),
            ),
            profile: VariantProfile {
                codec,
                container,
                headers: ctx.headers.clone(),
                bus: ctx.bus.clone(),
            },
            seek: VariantSeek::new(HlsVariant::NO_SEEK_TAIL),
            segments: VariantSegments::new(ctx.scope.clone(), init, segments),
        })
    }
}

impl HlsVariant {
    pub(super) const NO_SEEK_TAIL: u32 = u32::MAX;

    pub(crate) fn event_bus(&self) -> EventBus {
        self.profile.bus.clone()
    }

    /// Builds per-segment metadata. `#EXT-X-BYTERANGE` supplies an exact
    /// media-segment length when present; all other playlists get a non-exact
    /// routeable placeholder until body commit or lazy probe publishes the
    /// final length.
    fn build_segment_entries(
        playlist_state: &PlaylistState,
        decrypt_contexts: &[Option<DecryptContext>],
        variant_idx: usize,
        ctx: &PlanCtx,
    ) -> Vec<Segment> {
        let scope = &ctx.scope;
        let Some(num) = playlist_state.num_segments(variant_idx) else {
            return Vec::new();
        };
        let bandwidth_bps = playlist_state.variant_bandwidth_bps(variant_idx);
        let mut decode_time = Duration::ZERO;
        let mut entries = Vec::with_capacity(num);
        for seg_idx in 0..num {
            let Some(url) = playlist_state.segment_url(variant_idx, seg_idx) else {
                break;
            };
            let duration = playlist_state
                .segment_decode_range(variant_idx, seg_idx)
                .map_or(Duration::ZERO, |(start, end)| end.saturating_sub(start));
            let decrypt_ctx = decrypt_contexts.get(seg_idx).cloned().flatten();
            let size = playlist_state
                .segment_byte_range_len(variant_idx, seg_idx)
                .map_or_else(
                    || SegmentSize::placeholder(segment_placeholder_size(duration, bandwidth_bps)),
                    SegmentSize::seed,
                );
            entries.push(Segment::Media(MediaSegment {
                resource_id: scope.key_for(&url),
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
}
