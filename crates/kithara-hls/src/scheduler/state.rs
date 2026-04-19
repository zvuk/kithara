use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
};

use kithara_abr::{AbrController, AbrDecision, AbrReason, ThroughputEstimator};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{EventBus, HlsEvent, SeekEpoch};
use kithara_platform::time::Instant;
use tracing::debug;

use super::{
    cursor::DownloadCursor,
    helpers::{classify_layout_transition, is_cross_codec_switch},
};
use crate::{
    HlsError,
    coord::HlsCoord,
    ids::{SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::{SegmentData, StreamIndex},
};

/// HLS downloader: fetches segments and maintains ABR state.
pub(crate) struct HlsScheduler {
    pub(crate) active_seek_epoch: SeekEpoch,
    /// Direct disk-cache access — no `FetchManager` wrapper.
    pub(crate) backend: AssetStore<DecryptContext>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) cursor: DownloadCursor<SegmentIndex>,
    pub(crate) force_init_for_seek: bool,
    pub(crate) sent_init_for_variant: HashSet<VariantIndex>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold (segments ahead of reader). None = no limit.
    /// For ephemeral backends, derived from cache capacity to prevent
    /// evicting segments the reader still needs.
    pub(crate) look_ahead_segments: Option<usize>,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
    /// Variant the downloader is currently targeting. Used for transition
    /// classification instead of shared `layout_variant` to avoid racing
    /// with the decoder thread.
    pub(crate) download_variant: VariantIndex,
    /// True while `poll_next` is filling a layout-variant gap after ABR moved
    /// to a different variant. Prevents ABR from overriding the variant
    /// on subsequent `poll_next` cycles (avoids hot loop: ABR cached commit
    /// → tail gap detect → reset → ABR override → repeat).
    pub(crate) filling_layout_gap: bool,
    /// After a demand, caps cursor advancement so prefetch doesn't evict
    /// the demanded segment from an ephemeral LRU. Set to
    /// `demand_seg + look_ahead_segments` when a demand is processed;
    /// cleared on the next seek (new epoch) or when reader advances past.
    pub(crate) demand_throttle_until: Option<usize>,
    /// Highest cached-segment count already announced via
    /// `SegmentComplete { cached: true }` per variant. Prevents
    /// `apply_cached_segment_progress` from re-publishing the same events
    /// on every `poll_next` tick.
    pub(crate) announced_cached_count: HashMap<VariantIndex, usize>,
    /// Segments whose `FetchCmd` has been emitted and whose `on_complete`
    /// has not yet fired. Used by `process_demand` to skip rewinding the
    /// cursor onto an in-flight segment (which would issue a duplicate
    /// `FetchCmd` that races the original writer on the same cached
    /// `AssetResource`). Cleared on new seek epoch (the old epoch's
    /// cancel token drops any in-flight fetches).
    pub(crate) in_flight_segments: HashSet<(VariantIndex, SegmentIndex)>,
}

/// Maximum initial segment index for verbose logging.
pub(super) const VERBOSE_SEGMENT_LIMIT: usize = 8;

impl HlsScheduler {
    pub(crate) fn layout_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
    }

    pub(crate) fn current_segment_index(&self) -> SegmentIndex {
        self.cursor.fill_next()
    }

    pub(super) fn num_segments(&self, variant: VariantIndex) -> Option<usize> {
        self.playlist_state.num_segments(variant)
    }

    pub(super) fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    pub(crate) fn gap_scan_start_segment(&self) -> SegmentIndex {
        self.cursor.fill_floor()
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
            .map_or(0, |seg| seg.segment_index)
    }

    pub(crate) fn reset_cursor(&mut self, segment_index: SegmentIndex) {
        self.cursor.reset_fill(segment_index);
    }

    pub(crate) fn advance_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.cursor.advance_fill_to(segment_index);
    }

    pub(crate) fn rewind_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.cursor.rewind_fill_to(segment_index);
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
        let current_variant = self.abr.get_current_variant_index();
        variant == current_variant && segment_index < self.gap_scan_start_segment()
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "layout scan must hold the StreamIndex lock while item_range walks the current map"
    )]
    pub(super) fn switched_layout_anchor(&self) -> Option<(VariantIndex, u64)> {
        let variant = self.abr.get_current_variant_index();
        let anchor = {
            let segments = self.segments.lock_sync();
            let vs = segments.variant_segments(variant)?;
            vs.iter().find_map(|(segment_index, _data)| {
                segments
                    .item_range((variant, segment_index))
                    .map(|range| (segment_index, range.start))
            })
        };
        let (segment_index, byte_offset) = anchor?;
        ((segment_index > 0) || (byte_offset > 0)).then_some((variant, byte_offset))
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

    pub(crate) fn classify_variant_transition(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> (bool, bool) {
        classify_layout_transition(self.download_variant, variant, segment_index)
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

        self.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);
        // Drop stale entries — the prior epoch's FetchCmds are cancelled
        // by the epoch cancel token, so none of them will fire on_complete
        // in the new epoch.
        self.in_flight_segments.clear();

        self.force_init_for_seek = self
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
        self.filling_layout_gap = false;

        let current_variant = self.abr.get_current_variant_index();
        if current_variant != variant {
            self.abr.apply(
                &AbrDecision {
                    target_variant_index: variant,
                    reason: AbrReason::ManualOverride,
                    changed: true,
                },
                Instant::now(),
            );
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

    pub(crate) fn variant_has_init(&self, variant: usize) -> bool {
        self.playlist_state.init_url(variant).is_some()
    }

    pub(crate) fn publish_download_error(&self, context: &str, error: &HlsError) {
        debug!(?error, context, "hls downloader error");
        self.bus.publish(HlsEvent::DownloadError {
            error: format!("{context}: {error}"),
        });
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
}
