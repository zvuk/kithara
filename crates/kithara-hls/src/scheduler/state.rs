use std::{
    collections::{HashMap, HashSet},
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
use tracing::debug;

use super::{
    cursor::DownloadCursor,
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
    pub(crate) cursor: DownloadCursor<SegmentIndex>,
    pub(crate) active_seek_epoch: SeekEpoch,
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
    /// Segments whose `FetchCmd` has been emitted and whose `on_complete`
    /// has not yet fired. Used by `process_demand` to skip rewinding the
    /// cursor onto an in-flight segment (which would issue a duplicate
    /// `FetchCmd` that races the original writer on the same cached
    /// `AssetResource`). Cleared on new seek epoch (the old epoch's
    /// cancel token drops any in-flight fetches).
    pub(crate) in_flight_segments: HashSet<(VariantIndex, SegmentIndex)>,
    pub(crate) sent_init_for_variant: HashSet<VariantIndex>,
    /// After a demand, caps cursor advancement so prefetch doesn't evict
    /// the demanded segment from an ephemeral LRU. Set to
    /// `demand_seg + look_ahead_segments` when a demand is processed;
    /// cleared on the next seek (new epoch) or when reader advances past.
    pub(crate) demand_throttle_until: Option<usize>,
    /// True while `poll_next` is filling a layout-variant gap after ABR moved
    /// to a different variant. Prevents ABR from overriding the variant
    /// on subsequent `poll_next` cycles (avoids hot loop: ABR cached commit
    /// → tail gap detect → reset → ABR override → repeat).
    pub(crate) filling_layout_gap: bool,
    pub(crate) force_init_for_seek: bool,
}

/// HLS downloader: fetches segments and maintains ABR state.
pub(crate) struct HlsScheduler {
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
    /// Variant the downloader is currently targeting. Used for transition
    /// classification instead of shared `layout_variant` to avoid racing
    /// with the decoder thread.
    pub(crate) download_variant: VariantIndex,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
    pub(crate) runtime: SchedulerRuntime,
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
        // Segment-based throttle: only for ephemeral backends where LRU eviction
        // destroys data. Disk backends don't need this — files survive eviction.
        // Each fMP4 segment uses up to SLOTS_PER_SEGMENT LRU slots (init + media).
        let look_ahead_segments = if backend.is_ephemeral() {
            const SLOTS_PER_SEGMENT: usize = 2;
            let cache_cap = config.store.effective_cache_capacity().get();
            Some((cache_cap.saturating_sub(SLOTS_PER_SEGMENT) / SLOTS_PER_SEGMENT).max(1))
        } else {
            None
        };
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
            download_variant: initial_variant,
            runtime: SchedulerRuntime::default(),
        }
    }

    /// Mint an `HlsSource` that shares this scheduler's cache, segment
    /// index, and event bus. The scheduler stays the authoritative writer
    /// for the shared state; the returned source reads from it.
    pub(crate) fn spawn_source(&self, reader_segment: Arc<AtomicUsize>) -> HlsSource {
        HlsSource {
            coord: Arc::clone(&self.coord),
            backend: self.backend.clone(),
            segments: Arc::clone(&self.segments),
            playlist_state: Arc::clone(&self.playlist_state),
            bus: self.bus.clone(),
            reader_segment,
            segmented_view: HlsSegmentView::new(
                Arc::clone(&self.playlist_state),
                Arc::clone(&self.segments),
            ),
            variant_fence: None,
            _hls_peer: None,
            _peer_handle: None,
        }
    }

    pub(crate) fn advance_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.runtime.cursor.advance_fill_to(segment_index);
    }

    pub(crate) fn classify_variant_transition(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> (bool, bool) {
        classify_layout_transition(self.download_variant, variant, segment_index)
    }

    pub(crate) fn current_segment_index(&self) -> SegmentIndex {
        self.runtime.cursor.fill_next()
    }

    pub(super) fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    pub(crate) fn gap_scan_start_segment(&self) -> SegmentIndex {
        self.runtime.cursor.fill_floor()
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
        // Network errors are reported through `DownloaderEvent::RequestFailed`
        // — do not duplicate them on `HlsEvent::Error`.
        let public = match error {
            HlsError::Net(_) => return,
            HlsError::PlaylistParse(msg) => PublicHlsError::Playlist(format!("{context}: {msg}")),
            HlsError::KeyProcessing(msg) => PublicHlsError::Decryption(format!("{context}: {msg}")),
            other => PublicHlsError::Other(format!("{context}: {other}")),
        };
        self.bus.publish(HlsEvent::Error { error: public });
    }

    /// Segment index the reader is currently inside, in the layout variant.
    ///
    /// Tail-state gap scans must not consider segments strictly behind the
    /// reader as "missing": in an ephemeral LRU these were evicted by design
    /// once played, and re-fetching them only evicts the segments the reader
    /// is actively reading next — a hot loop.
    pub(crate) fn reader_segment_floor(&self) -> SegmentIndex {
        let byte_pos = self.coord.timeline().byte_position();
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

    pub(crate) fn reset_cursor(&mut self, segment_index: SegmentIndex) {
        self.runtime.cursor.reset_fill(segment_index);
    }

    pub(super) fn reset_for_seek_epoch(
        &mut self,
        seek_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    ) {
        let previous_variant = self.layout_variant();

        // ABR lifecycle is owned by `make_abr_decision` — it acquires the
        // lock the first time it observes `is_seek_pending()` and releases
        // it the first time the pending flag clears. Reset no longer adds
        // its own lock: doing so double-counts the refcount and leaks a
        // permanent lock after seek completes.

        self.runtime.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);
        // Drop stale entries — the prior epoch's FetchCmds are cancelled
        // by the epoch cancel token, so none of them will fire on_complete
        // in the new epoch.
        self.runtime.in_flight_segments.clear();

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

        self.download_variant = variant;
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

    pub(crate) fn rewind_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.runtime.cursor.rewind_fill_to(segment_index);
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
