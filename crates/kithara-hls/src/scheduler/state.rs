use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_abr::{AbrDecision, AbrState};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{AbrReason, EventBus, HlsError as PublicHlsError, HlsEvent, SeekEpoch};
use kithara_platform::{Mutex, time::Instant};
use kithara_stream::Timeline;
use kithara_test_utils::kithara;
use tracing::debug;

use super::{
    cursor::{DownloadCursor, VariantDownloadState},
    helpers::{classify_layout_transition, is_cross_codec_switch},
};
use crate::{
    HlsError,
    config::HlsConfig,
    coord::HlsCoord,
    ids::{SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState},
    source::{HlsSegmentView, HlsSource},
    stream_index::{SegmentData, StreamIndex},
};

/// Mutable runtime state of `HlsScheduler` whose initial value is the
/// `Default` of every field — empty maps/sets, zero counters, `false`
/// flags, an unset throttle, a cursor at zero. Bundled into a sub-struct
/// so `HlsScheduler::new` can spell `runtime: SchedulerRuntime::default()`
/// instead of listing each zero-valued field explicitly, and so adding a
/// new runtime flag does not ripple into every constructor call site.
#[derive(Default)]
#[non_exhaustive]
pub(crate) struct SchedulerRuntime {
    /// Highest cached-segment count already announced via
    /// `SegmentComplete { cached: true }` per variant. Prevents
    /// `apply_cached_segment_progress` from re-publishing the same events
    /// on every `poll_next` tick.
    pub(crate) announced_cached_count: HashMap<VariantIndex, usize>,
    /// Highest cached-segment count already populated into `StreamIndex`
    /// by `populate_cached_segments_if_needed` per variant. Prevents
    /// repeated full-playlist disk scans on every `poll_next` tick — the
    /// scan is heavy (per-segment `resource_state` + `commit_segment`)
    /// and unconditionally fires `coord.condvar.notify_all()` which wakes
    /// the audio worker, which polls again, which re-scans: a CPU-bound
    /// hot loop that exhausts the test budget under stress.
    ///
    /// Only meaningful for non-ephemeral backends — ephemeral stores
    /// short-circuit `populate_cached_segments` directly.
    pub(crate) populated_cached_count: HashMap<VariantIndex, usize>,
    /// Per-variant download bookkeeping (in-flight segments, init-sent
    /// flag). Phase 1 invariant: at most one entry, keyed by the current
    /// `primary_variant`. Phase 3 will add a second entry for the
    /// blender period.
    ///
    /// Use `HlsScheduler::is_in_flight` / `mark_in_flight` /
    /// `unmark_in_flight` / `mark_init_sent` /
    /// `clear_in_flight_for_seek` instead of touching this map directly
    /// — the helpers create per-variant entries lazily and keep the
    /// invariant centralized.
    pub(crate) active_downloads: BTreeMap<VariantIndex, VariantDownloadState>,
    /// After a demand, caps cursor advancement so prefetch doesn't evict
    /// the demanded segment from an ephemeral LRU. Set to
    /// `demand_seg + look_ahead_segments` when a demand is processed;
    /// cleared on the next seek (new epoch) or when reader advances past.
    pub(crate) demand_throttle_until: Option<usize>,
    pub(crate) active_seek_epoch: SeekEpoch,
    /// True while `poll_next` is filling a layout-variant gap after ABR moved
    /// to a different variant. Prevents ABR from overriding the variant
    /// on subsequent `poll_next` cycles (avoids hot loop: ABR cached commit
    /// → tail gap detect → reset → ABR override → repeat).
    pub(crate) filling_layout_gap: bool,
    pub(crate) force_init_for_seek: bool,
}

/// HLS downloader: fetches segments and maintains ABR state.
pub struct HlsScheduler {
    /// Shared ABR state — same `Arc` the owning `HlsPeer` exposes via
    /// `Abr::state()`. Lock/unlock drives the seek-no-switch invariant;
    /// `current_variant_index()` is the source of truth for the scheduler.
    pub(crate) abr: Arc<AbrState>,
    /// One past the highest segment index ever committed — shared with the
    /// owning `HlsPeer::committed_segment` cursor so `HlsPeer::progress`
    /// can expose `download_head_playback_time` to the ABR controller.
    pub(crate) committed_segment: Arc<AtomicUsize>,
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
    /// Direct disk-cache access — no `FetchManager` wrapper.
    pub(crate) backend: AssetStore<DecryptContext>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold (segments ahead of reader). None = no limit.
    /// For ephemeral backends, derived from cache capacity to prevent
    /// evicting segments the reader still needs.
    pub(crate) look_ahead_segments: Option<usize>,
    pub(crate) runtime: SchedulerRuntime,
    /// Variant the downloader is currently targeting. Used for transition
    /// classification instead of shared `layout_variant` to avoid racing
    /// with the decoder thread. Phase 1 invariant: also the only key in
    /// `runtime.active_downloads`. Phase 3 will introduce a separate
    /// `secondary_variant` slot for the blender period.
    pub(crate) primary_variant: VariantIndex,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
}

/// Maximum initial segment index for verbose logging.
pub(super) const VERBOSE_SEGMENT_LIMIT: usize = 8;

impl HlsScheduler {
    pub(crate) fn new(
        backend: AssetStore<DecryptContext>,
        playlist_state: Arc<PlaylistState>,
        abr: Arc<AbrState>,
        timeline: Timeline,
        bus: EventBus,
        config: &HlsConfig,
        committed_segment: Arc<AtomicUsize>,
    ) -> Self {
        let cancel = config.cancel.clone().unwrap_or_default();
        timeline.set_total_duration(playlist_state.track_duration());
        let coord = Arc::new(HlsCoord::new(cancel, timeline, Arc::clone(&abr)));
        let num_variants = playlist_state.num_variants();
        let num_segments = playlist_state.num_segments(0).unwrap_or(0);
        let initial_variant = coord.variant_index();
        let mut stream_index = StreamIndex::new(num_variants, num_segments);
        if initial_variant < num_variants {
            stream_index.set_layout_variant(initial_variant);
        }
        let segments = Arc::new(Mutex::new(stream_index));
        let look_ahead_segments = if backend.is_ephemeral() {
            const SLOTS_PER_SEGMENT: usize = 2;
            let cache_cap = config.store.effective_cache_capacity().get();
            Some((cache_cap.saturating_sub(SLOTS_PER_SEGMENT) / SLOTS_PER_SEGMENT).max(1))
        } else {
            None
        };
        let mut runtime = SchedulerRuntime::default();
        // Pre-populate the primary variant entry so `current_only` and
        // the cursor helpers always observe at least one
        // `VariantDownloadState`. Phase 1 invariant: this is the only
        // entry until Phase 3 adds a second one for the blender.
        runtime
            .active_downloads
            .insert(initial_variant, VariantDownloadState::default());
        Self {
            look_ahead_segments,
            committed_segment,
            backend,
            playlist_state,
            abr,
            coord,
            segments,
            bus,
            prefetch_count: config.download_batch_size.max(1),
            primary_variant: initial_variant,
            runtime,
        }
    }

    pub(crate) fn advance_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.primary_cursor_mut().advance_fill_to(segment_index);
    }

    /// Drop every per-variant in-flight set. Called on seek-epoch reset
    /// — the old epoch's cancel token drops any in-flight `FetchCmd`s,
    /// so the bookkeeping must follow.
    pub(crate) fn clear_in_flight_for_seek(&mut self) {
        for state in self.runtime.active_downloads.values_mut() {
            state.in_flight.clear();
        }
    }

    pub(crate) fn classify_variant_transition(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> (bool, bool) {
        classify_layout_transition(self.primary_variant, variant, segment_index)
    }

    pub(crate) fn current_segment_index(&self) -> SegmentIndex {
        self.current_only().cursor.fill_next()
    }

    /// `true` if a `FetchCmd` for `(variant, segment_index)` was emitted
    /// and its `on_complete` has not fired yet.
    pub(crate) fn is_in_flight(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        self.runtime
            .active_downloads
            .get(&variant)
            .is_some_and(|s| s.in_flight.contains(&segment_index))
    }

    /// Record that a `FetchCmd` for `(variant, segment_index)` has been
    /// emitted. Lazily creates the per-variant entry.
    pub(crate) fn mark_in_flight(&mut self, variant: VariantIndex, segment_index: SegmentIndex) {
        self.runtime
            .active_downloads
            .entry(variant)
            .or_default()
            .in_flight
            .insert(segment_index);
    }

    /// Mark the init segment for `variant` as committed. Lazily creates
    /// the per-variant entry.
    pub(crate) fn mark_init_sent(&mut self, variant: VariantIndex) {
        self.runtime
            .active_downloads
            .entry(variant)
            .or_default()
            .init_sent = true;
    }

    /// Drop the in-flight record for `(variant, segment_index)` once
    /// `on_complete` fires. No-op when the entry is absent (legitimate
    /// after a seek-epoch reset cleared the set).
    pub(crate) fn unmark_in_flight(&mut self, variant: VariantIndex, segment_index: SegmentIndex) {
        if let Some(state) = self.runtime.active_downloads.get_mut(&variant) {
            state.in_flight.remove(&segment_index);
        }
    }

    /// The single active download track's `VariantDownloadState`.
    /// Phase 1 invariant: `runtime.active_downloads` has exactly one
    /// entry, keyed by `primary_variant`. The constructor pre-populates
    /// it; subsequent variant switches in
    /// `reset_for_seek_epoch` / `apply_variant_readiness` /
    /// `handle_midstream_switch` keep the invariant.
    pub(crate) fn current_only(&self) -> &VariantDownloadState {
        self.runtime
            .active_downloads
            .get(&self.primary_variant)
            .expect(
                "Phase 1 invariant: active_downloads always has an entry \
                 for primary_variant (HlsScheduler::new pre-populates it)",
            )
    }

    /// Mutable cursor for the current `primary_variant`. Lazily creates
    /// the per-variant entry with default cursor `(0, 0)`.
    pub(crate) fn primary_cursor_mut(&mut self) -> &mut DownloadCursor<SegmentIndex> {
        let variant = self.primary_variant;
        self.cursor_mut_for(variant)
    }

    /// Switch the active download track to `variant` while keeping the
    /// Phase 1 invariant that `active_downloads` always carries an
    /// entry for `primary_variant`. Lazy-creates the new entry if it
    /// is absent so the next `current_only` / `current_segment_index`
    /// read does not panic.
    ///
    /// Pre-existing entries — both for the outgoing variant and for
    /// `variant` itself (populated earlier by, e.g.,
    /// `handle_midstream_switch::cursor_mut_for(target)`) — are
    /// preserved verbatim. Phase 1 is non-behavioural; explicit
    /// teardown of stale entries is deferred to Phase 3 where the
    /// blender introduces a second slot with its own lifecycle.
    pub(crate) fn set_primary_variant(&mut self, variant: VariantIndex) {
        self.primary_variant = variant;
        self.runtime.active_downloads.entry(variant).or_default();
    }

    /// Mutable cursor for an explicit variant. Used by callers that
    /// need to position the cursor on a variant they are about to
    /// install as `primary_variant` (the only such caller today is
    /// `handle_midstream_switch`).
    pub(crate) fn cursor_mut_for(
        &mut self,
        variant: VariantIndex,
    ) -> &mut DownloadCursor<SegmentIndex> {
        &mut self
            .runtime
            .active_downloads
            .entry(variant)
            .or_default()
            .cursor
    }

    pub(super) fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    pub(crate) fn gap_scan_start_segment(&self) -> SegmentIndex {
        self.current_only().cursor.fill_floor()
    }

    pub(super) fn is_below_switch_floor(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> bool {
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) {
            return false;
        }
        let layout = self.segments.lock_sync().layout_variant();
        if variant == layout {
            return false;
        }
        let current_variant = self.abr.current_variant_index();
        variant == current_variant && segment_index < self.gap_scan_start_segment()
    }

    pub(super) fn is_stale_cross_codec(&self, variant: VariantIndex, seg_idx: usize) -> bool {
        let Some((current_variant, anchor_offset)) = self.switched_layout_anchor() else {
            return false;
        };
        if variant == current_variant
            || !is_cross_codec_switch(&self.playlist_state, current_variant, variant)
        {
            return false;
        }

        let fetch_offset = self
            .playlist_state
            .segment_byte_offset(variant, seg_idx)
            .unwrap_or_else(|| self.coord.timeline().download_position());

        fetch_offset >= anchor_offset
    }

    pub(crate) fn layout_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
    }

    pub(super) fn num_segments(&self, variant: VariantIndex) -> Option<usize> {
        self.playlist_state.num_segments(variant)
    }

    pub(crate) fn publish_download_error(&self, context: &str, error: &HlsError) {
        debug!(?error, context, "hls downloader error");
        let public = match error {
            HlsError::Net(_) => return,
            HlsError::PlaylistParse(msg) => PublicHlsError::Playlist(format!("{context}: {msg}")),
            HlsError::KeyProcessing(msg) => PublicHlsError::Decryption(format!("{context}: {msg}")),
            other => PublicHlsError::Other(format!("{context}: {other}")),
        };
        self.bus.publish(HlsEvent::Error { error: public });
    }

    /// Publish a download error and, if the error is permanent, record it
    /// on the affected segment slot so the reader's `wait_range` can exit
    /// instead of spinning until the hang detector fires.
    ///
    /// "Permanent" means the same retry cannot succeed without external
    /// intervention: bad DRM key (auth/expiration), broken playlist,
    /// missing variant/segment in metadata, or unparseable URL. Network
    /// errors stay on the existing retry path (`peer.rs`'s `rewinding
    /// cursor` flow handles those).
    pub(crate) fn publish_segment_failure(
        &self,
        context: &str,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        error: HlsError,
    ) {
        self.publish_download_error(context, &error);
        if !is_permanent_error(&error) {
            return;
        }
        self.segments
            .lock_sync()
            .mark_segment_failed(variant, segment_index, Arc::new(error));
        self.coord.condvar.notify_all();
    }

    /// Segment index the reader is currently inside, in the layout variant.
    ///
    /// Tail-state gap scans must not consider segments strictly behind the
    /// reader as "missing": in an ephemeral LRU these were evicted by design
    /// once played, and re-fetching them only evicts the segments the reader
    /// is actively reading next — a hot loop.
    pub(crate) fn reader_segment_floor(&self) -> SegmentIndex {
        let byte_pos = self.coord.position();
        if byte_pos == 0 {
            return 0;
        }
        let layout = self.layout_variant();
        self.segments
            .lock_sync()
            .find_at_offset_in(layout, byte_pos)
            .filter(|seg| seg.data.is_some())
            .map_or(0, |seg| seg.segment_index)
    }

    #[kithara::probe(segment_index, caller)]
    pub(crate) fn reset_cursor(&mut self, segment_index: SegmentIndex) {
        self.primary_cursor_mut().reset_fill(segment_index);
    }

    pub(super) fn reset_for_seek_epoch(
        &mut self,
        seek_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    ) {
        let previous_variant = self.layout_variant();

        // `primary_variant` switches first so the cursor reset below
        // and `clear_in_flight_for_seek` operate on the post-seek
        // variant entry rather than the outgoing one. The single global
        // cursor in the pre-Phase-1 model was variant-agnostic; the
        // per-variant model needs the active key to be in place before
        // we touch the corresponding `VariantDownloadState`.
        self.set_primary_variant(variant);
        self.runtime.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);
        self.clear_in_flight_for_seek();

        self.runtime.force_init_for_seek = self
            .playlist_state
            .variant_codec(previous_variant)
            .zip(self.playlist_state.variant_codec(variant))
            .is_some_and(|(from, to)| from != to);

        if previous_variant == variant {
            let watermark = self.coord.timeline().download_position();
            let target_offset = self
                .playlist_state
                .segment_byte_offset(variant, segment_index)
                .unwrap_or(0);
            self.coord
                .timeline()
                .set_download_position(watermark.max(target_offset));
        } else {
            let download_position = self
                .playlist_state
                .segment_byte_offset(variant, segment_index)
                .unwrap_or(0);
            self.coord
                .timeline()
                .set_download_position(download_position);
        }

        self.runtime.filling_layout_gap = false;

        let current_variant = self.abr.current_variant_index();
        if current_variant != variant {
            self.abr.apply(
                &AbrDecision {
                    target_variant_index: variant,
                    reason: AbrReason::ManualOverride,
                    did_change: true,
                },
                Instant::now(),
            );
        }
    }

    #[kithara::probe(segment_index, caller)]
    pub(crate) fn rewind_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.primary_cursor_mut().rewind_fill_to(segment_index);
    }

    pub(super) fn segment_resources_available(&self, data: &SegmentData) -> bool {
        if data.init_len > 0
            && let Some(ref init_url) = data.init_url
            && !self
                .backend
                .contains_range(&ResourceKey::from_url(init_url), 0..data.init_len)
        {
            return false;
        }
        self.backend
            .contains_range(&ResourceKey::from_url(&data.media_url), 0..data.media_len)
    }

    /// Mint an `HlsSource` that shares this scheduler's cache, segment
    /// index, and event bus. The scheduler stays the authoritative writer
    /// for the shared state; the returned source reads from it.
    pub(crate) fn spawn_source(&self, reader_segment: Arc<AtomicUsize>) -> HlsSource {
        HlsSource {
            reader_segment,
            coord: Arc::clone(&self.coord),
            backend: self.backend.clone(),
            segments: Arc::clone(&self.segments),
            playlist_state: Arc::clone(&self.playlist_state),
            bus: self.bus.clone(),
            segmented_view: HlsSegmentView::new(
                Arc::clone(&self.playlist_state),
                Arc::clone(&self.segments),
            ),
            variant_fence: None,
            _hls_peer: None,
            _peer_handle: None,
        }
    }

    pub(crate) fn switch_needs_init(
        &self,
        variant: usize,
        _segment_index: usize,
        is_variant_switch: bool,
    ) -> bool {
        if !is_variant_switch {
            return false;
        }

        let previous = self.layout_variant();
        previous != variant && is_cross_codec_switch(&self.playlist_state, previous, variant)
    }

    pub(super) fn switched_layout_anchor(&self) -> Option<(VariantIndex, u64)> {
        let variant = self.abr.current_variant_index();
        let segments = self.segments.lock_sync();
        let vs = segments.variant_segments(variant)?;
        let anchor = vs.iter().find_map(|(segment_index, _data)| {
            segments
                .item_range((variant, segment_index))
                .map(|range| (segment_index, range.start))
        });
        drop(segments);
        let (segment_index, byte_offset) = anchor?;
        ((segment_index > 0) || (byte_offset > 0)).then_some((variant, byte_offset))
    }

    pub(crate) fn variant_has_init(&self, variant: usize) -> bool {
        self.playlist_state.init_url(variant).is_some()
    }
}

fn is_permanent_error(error: &HlsError) -> bool {
    matches!(
        error,
        HlsError::KeyProcessing(_)
            | HlsError::PlaylistParse(_)
            | HlsError::VariantNotFound(_)
            | HlsError::SegmentNotFound(_)
            | HlsError::InvalidUrl(_)
    )
}
