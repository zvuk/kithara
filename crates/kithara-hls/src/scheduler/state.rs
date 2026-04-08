use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};

use kithara_abr::{AbrController, AbrDecision, AbrReason, ThroughputEstimator};
use kithara_assets::{AssetResourceState, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{EventBus, HlsEvent, SeekEpoch};
use kithara_platform::time::Instant;
use kithara_storage::ResourceExt;
use tracing::debug;

use super::{
    cursor::DownloadCursor,
    helpers::{classify_layout_transition, is_cross_codec_switch},
    trait_impl::HlsFetch,
};
use crate::{
    HlsError,
    coord::HlsCoord,
    ids::{SegmentIndex, VariantIndex},
    loading::SizeMapProbe,
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::{SegmentData, StreamIndex},
};

/// HLS downloader: fetches segments and maintains ABR state.
pub(crate) struct HlsScheduler {
    pub(crate) active_seek_epoch: SeekEpoch,
    /// Direct disk-cache access — no `FetchManager` wrapper.
    pub(crate) backend: AssetStore<DecryptContext>,
    /// HEAD-probe helper for size-map building.
    pub(crate) size_probe: SizeMapProbe,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) cursor: DownloadCursor<SegmentIndex>,
    pub(crate) force_init_for_seek: bool,
    pub(crate) sent_init_for_variant: HashSet<VariantIndex>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold (bytes). None = no byte-based backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
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
}

/// Maximum number of plans to log at debug level.
pub(super) const MAX_LOG_PLANS: usize = 4;

/// Maximum initial segment index for verbose logging.
pub(super) const VERBOSE_SEGMENT_LIMIT: usize = 8;

impl HlsScheduler {
    pub(super) fn layout_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
    }

    pub(super) fn current_segment_index(&self) -> SegmentIndex {
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

    pub(super) fn gap_scan_start_segment(&self) -> SegmentIndex {
        self.cursor.fill_floor()
    }

    pub(super) fn reset_cursor(&mut self, segment_index: SegmentIndex) {
        self.cursor.reset_fill(segment_index);
    }

    pub(super) fn advance_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.cursor.advance_fill_to(segment_index);
    }

    pub(super) fn rewind_current_segment_index(&mut self, segment_index: SegmentIndex) {
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

    pub(super) fn is_stale_cross_codec_fetch(&self, fetch: &HlsFetch) -> bool {
        let Some((current_variant, anchor_offset)) = self.switched_layout_anchor() else {
            return false;
        };
        if fetch.variant == current_variant
            || !is_cross_codec_switch(&self.playlist_state, current_variant, fetch.variant)
        {
            return false;
        }

        let seg_idx = fetch.segment.media_index().unwrap_or(0);
        let fetch_offset = self
            .playlist_state
            .segment_byte_offset(fetch.variant, seg_idx)
            .unwrap_or_else(|| self.coord.timeline().download_position());

        fetch_offset >= anchor_offset
    }

    pub(super) fn classify_variant_transition(
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

        self.abr.lock();

        self.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);

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

    pub(super) fn switch_needs_init(
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

    pub(super) fn variant_has_init(&self, variant: usize) -> bool {
        self.playlist_state.init_url(variant).is_some()
    }

    pub(super) fn publish_download_error(&self, context: &str, error: &HlsError) {
        debug!(?error, context, "hls downloader error");
        self.bus.publish(HlsEvent::DownloadError {
            error: format!("{context}: {error}"),
        });
    }

    pub(super) fn reader_segment_hint(&self, variant: usize) -> usize {
        let reader_offset = self.coord.timeline().byte_position();
        self.playlist_state
            .find_segment_at_offset(variant, reader_offset)
            .unwrap_or_else(|| self.current_segment_index())
    }

    pub(super) fn resource_covers_len(&self, key: &ResourceKey, len: u64) -> bool {
        if len == 0 {
            return true;
        }

        match self.backend.resource_state(key) {
            Ok(AssetResourceState::Committed {
                final_len: Some(final_len),
            }) => final_len >= len,
            Ok(AssetResourceState::Committed { .. }) => self
                .backend
                .open_resource(key)
                .is_ok_and(|resource| resource.contains_range(0..len)),
            _ => false,
        }
    }

    pub(super) fn segment_resources_available(&self, data: &SegmentData) -> bool {
        if data.init_len > 0
            && let Some(ref init_url) = data.init_url
            && !self.resource_covers_len(&ResourceKey::from_url(init_url), data.init_len)
        {
            return false;
        }
        self.resource_covers_len(&ResourceKey::from_url(&data.media_url), data.media_len)
    }

    pub(super) fn demand_init_evicted(&self, variant: usize, segment_index: usize) -> bool {
        let segments = self.segments.lock_sync();
        let Some(data) = segments.stored_segment(variant, segment_index) else {
            return false;
        };
        let init_len = data.init_len;
        if init_len == 0 {
            return false;
        }
        let Some(init_url) = data.init_url.clone() else {
            return false;
        };
        drop(segments);
        !self.resource_covers_len(&ResourceKey::from_url(&init_url), init_len)
    }
}
