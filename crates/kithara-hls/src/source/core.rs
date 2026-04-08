use std::{
    collections::HashSet,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use kithara_abr::{AbrController, AbrOptions, Variant};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{Mutex, time::Duration};
use kithara_stream::{DownloadCursor, LayoutIndex, StreamResult, Timeline};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::types::ReadSegment;
use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    downloader::HlsDownloader,
    ids::{SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState},
    segment_loader::SegmentLoader,
    stream_index::{SegmentData, StreamIndex},
    worker::WorkerGuard,
};

/// HLS source: provides random-access reading from loaded segments.
///
/// Holds an optional [`WorkerGuard`] that cancels+joins the background
/// download worker when the source is dropped.
pub struct HlsSource {
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) loader: Arc<SegmentLoader>,
    pub(crate) backend: AssetStore<DecryptContext>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<VariantIndex>,
    /// Worker guard. Dropped with this source, cancelling the worker.
    pub(crate) _worker: Option<WorkerGuard>,
    /// Dedup key for `wait_range_metadata_fallback` events. Encodes
    /// `(variant << 48) | offset` so the event fires at most once per
    /// unique (variant, offset) pair during a spin-wait loop.
    pub(crate) last_fallback_key: AtomicU64,
}

impl HlsSource {
    /// Set the worker guard (called after the worker is spawned).
    pub(crate) fn set_worker(&mut self, guard: WorkerGuard) {
        self._worker = Some(guard);
    }

    /// Current variant for source operations (read, seek, demand).
    ///
    /// Always uses `layout_variant` — ABR only affects playback via
    /// `media_info()` → `format_change` detection → layout switch.
    pub(super) fn resolve_current_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
    }

    pub(crate) fn can_cross_variant_without_reset(
        &self,
        from_variant: VariantIndex,
        to_variant: VariantIndex,
    ) -> bool {
        self.playlist_state.variant_codec(from_variant)
            == self.playlist_state.variant_codec(to_variant)
    }

    /// Returns the variant of the current committed byte layout.
    ///
    /// Priority: committed segment at reader position → last committed segment
    /// → `variant_fence` → `None` (forces Reset).
    ///
    /// Does NOT fall back to ABR hint — ABR hint is the *target*, not the
    /// *current* layout. Using it would misclassify cross-variant seeks as
    /// Preserve when `variant_fence` was cleared before `seek_time_anchor()`.
    pub(super) fn current_layout_variant(&self) -> Option<VariantIndex> {
        let pos = self.coord.timeline().byte_position();
        let segments = self.segments.lock_sync();
        if let Some(seg_ref) = segments.find_at_offset(pos) {
            return Some(seg_ref.variant);
        }
        // Fallback: check the last committed segment across all variants
        if segments.max_end_offset() > 0
            && let Some(seg_ref) =
                segments.find_at_offset(segments.max_end_offset().saturating_sub(1))
        {
            return Some(seg_ref.variant);
        }
        drop(segments);
        self.variant_fence
    }

    /// Returns `(variant, segment_index)` for the segment at the current reader position,
    /// falling back to the last committed segment if no segment covers the exact position.
    pub(super) fn current_loaded_segment_key(&self) -> Option<(VariantIndex, SegmentIndex)> {
        let reader_offset = self.coord.timeline().byte_position();
        let segments = self.segments.lock_sync();
        let result = segments
            .find_at_offset(reader_offset)
            .or_else(|| {
                let max = segments.max_end_offset();
                (max > 0)
                    .then(|| segments.find_at_offset(max.saturating_sub(1)))
                    .flatten()
            })
            .map(|seg_ref| (seg_ref.variant, seg_ref.segment_index));
        drop(segments);
        result
    }

    pub(super) fn current_segment_index(&self) -> Option<SegmentIndex> {
        self.current_loaded_segment_key()
            .map(|(_, seg_idx)| seg_idx)
    }

    pub(super) fn layout_segment_for_offset(
        &self,
        offset: u64,
    ) -> Option<(VariantIndex, SegmentIndex)> {
        self.segments
            .lock_sync()
            .segment_for_offset(offset, self.playlist_state.as_ref())
    }

    pub(super) fn byte_offset_for_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<u64> {
        let segments = self.segments.lock_sync();
        if let Some(range) =
            <StreamIndex as LayoutIndex>::item_range(&segments, (variant, segment_index))
        {
            return Some(range.start);
        }
        let layout_offset =
            segments.layout_offset_for_segment(segment_index, self.playlist_state.as_ref());
        drop(segments);
        self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .or(layout_offset)
    }

    pub(super) fn metadata_range_for_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<Range<u64>> {
        let start = self
            .playlist_state
            .segment_byte_offset(variant, segment_index)?;
        let end = self
            .playlist_state
            .segment_byte_offset(variant, segment_index + 1)
            .or_else(|| self.playlist_state.total_variant_size(variant))?;
        (end > start).then_some(start..end)
    }

    pub(super) fn init_segment_range_for_variant(
        &self,
        variant: VariantIndex,
    ) -> Option<Range<u64>> {
        let segments = self.segments.lock_sync();
        let vs = segments.variant_segments(variant)?;
        // Find first committed segment with init data
        let (seg_idx, seg_data) = vs.iter().find(|(_, data)| data.init_len > 0)?;

        // Always return a range starting from offset 0 (segment 0).
        // During midstream ABR switches the downloader commits a later segment
        // first (e.g. segment 15 with init). Using that segment's byte offset
        // as base_offset makes the decoder see only a tail of the stream,
        // causing "unexpected end of file" on late-track seeks.
        // Returning 0-based range ensures the decoder sees the full stream.
        // The pipeline waits (source_is_ready_for_boundary) until segment 0
        // is actually downloaded before creating the decoder.
        if seg_idx == 0 {
            let seg_range = <StreamIndex as LayoutIndex>::item_range(&segments, (variant, 0))?;
            if seg_data.init_url.is_none() {
                drop(segments);
                if let Some(metadata_range) = self.metadata_range_for_segment(variant, 0) {
                    return Some(metadata_range);
                }
            }
            return Some(seg_range.start..seg_range.end);
        }

        // Init-bearing segment is not segment 0 — use segment 0's range
        // (from byte map or metadata) so the decoder starts at offset 0.
        if let Some(seg0_range) = <StreamIndex as LayoutIndex>::item_range(&segments, (variant, 0))
        {
            return Some(seg0_range);
        }
        drop(segments);
        self.metadata_range_for_segment(variant, 0)
    }

    pub(super) fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    pub(super) fn is_past_eof(&self, segments: &StreamIndex, range: &Range<u64>) -> bool {
        let known_total = segments.effective_total(self.playlist_state.as_ref());
        let loaded_total = segments.max_end_offset();
        let effective_total = loaded_total.max(known_total);
        if effective_total == 0 || range.start < effective_total {
            return false;
        }

        true
    }

    pub(super) fn is_transient_empty_eof(&self, segments: &StreamIndex) -> bool {
        self.coord.timeline().eof() && segments.effective_total(self.playlist_state.as_ref()) == 0
    }

    pub(super) fn is_transient_demand_gap(&self) -> bool {
        let segments = self.segments.lock_sync();
        self.is_transient_empty_eof(&segments)
    }
}

/// Build an `HlsDownloader` + `HlsSource` pair from config.
pub(crate) fn build_pair(
    loader: Arc<SegmentLoader>,
    backend: AssetStore<DecryptContext>,
    downloader_handle: kithara_stream::dl::Downloader,
    variants: &[crate::parsing::VariantStream],
    config: &crate::config::HlsConfig,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
) -> (HlsDownloader, HlsSource) {
    let abr_variants: Vec<Variant> = variants
        .iter()
        .map(|v| Variant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
        })
        .collect();

    let cancel = config.cancel.clone().unwrap_or_default();
    let abr = match config.abr.clone() {
        Some(ctrl) => {
            ctrl.set_variants(abr_variants);
            ctrl
        }
        None => AbrController::new(AbrOptions {
            variants: abr_variants,
            ..AbrOptions::default()
        }),
    };
    let abr_variant_index = abr.variant_index_handle();
    let timeline = Timeline::new();
    timeline.set_total_duration(playlist_state.track_duration());
    let coord = Arc::new(HlsCoord::new(cancel, timeline, abr_variant_index));
    let num_variants = playlist_state.num_variants();
    let num_segments = playlist_state.num_segments(0).unwrap_or(0);
    let initial_variant = coord.abr_variant_index.load(Ordering::Acquire);
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

    let size_probe =
        crate::size_probe::SizeMapProbe::new(downloader_handle, config.headers.clone());
    let downloader = HlsDownloader {
        active_seek_epoch: 0,
        backend: backend.clone(),
        size_probe,
        playlist_state: Arc::clone(&playlist_state),
        cursor: DownloadCursor::fill(0),
        force_init_for_seek: false,
        sent_init_for_variant: HashSet::new(),
        abr,
        coord: Arc::clone(&coord),
        segments: Arc::clone(&segments),
        bus: bus.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        look_ahead_segments,
        prefetch_count: config.download_batch_size.max(1),
        download_variant: initial_variant,
    };

    let source = HlsSource {
        coord,
        loader,
        backend,
        segments,
        playlist_state,
        bus,
        variant_fence: None,
        _worker: None,
        last_fallback_key: AtomicU64::new(u64::MAX),
    };

    (downloader, source)
}
