use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{Arc, atomic::Ordering},
};

use kithara_abr::{AbrController, AbrOptions, Variant};
use kithara_assets::AssetStore;
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_platform::Mutex;
use kithara_stream::Timeline;

use crate::{
    coord::HlsCoord,
    ids::{SegmentIndex, VariantIndex},
    peer::HlsPeer,
    playlist::{PlaylistAccess, PlaylistState},
    scheduler::{DownloadCursor, HlsScheduler},
    stream_index::StreamIndex,
};

/// HLS source: provides random-access reading from loaded segments.
///
/// Reads bytes directly from the [`AssetStore`] backend — segment
/// downloading is the Downloader's job via `poll_next`, not the
/// source's. Holds an optional [`PeerHandle`](kithara_stream::dl::PeerHandle)
/// that cancels in-flight fetches when the source is dropped.
pub struct HlsSource {
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) backend: AssetStore<DecryptContext>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<VariantIndex>,
    /// HLS peer. Cloned from the Downloader's registry; `Drop` tears down
    /// its [`HlsState`] so the stashed [`SegmentLoader`]'s internal
    /// `PeerHandle` clones release `PeerInner.cancel`, letting the
    /// registry remove its own `Arc<HlsPeer>`. Without this the whole
    /// peer graph leaks until the Downloader itself is dropped.
    pub(crate) _hls_peer: Option<Arc<HlsPeer>>,
    /// Peer handle. Dropped with this source, cancelling the peer.
    pub(crate) _peer_handle: Option<kithara_stream::dl::PeerHandle>,
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        // Clear `HlsPeer::state` *before* `_peer_handle` and `_hls_peer`
        // drop — releasing the internal PeerHandle clones stored inside
        // the scheduler/loader so that the final external drop actually
        // brings `PeerInner`'s strong count to zero.
        if let Some(ref peer) = self._hls_peer {
            peer.teardown();
        }
    }
}

impl HlsSource {
    /// Set the peer handle (called after the peer is activated).
    pub(crate) fn set_peer_handle(&mut self, handle: kithara_stream::dl::PeerHandle) {
        self._peer_handle = Some(handle);
    }

    /// Store the `Arc<HlsPeer>` so `Drop` can tear down its state and
    /// break the `Registry → HlsPeer → HlsState::loader → PeerHandle`
    /// reference cycle.
    pub(crate) fn set_hls_peer(&mut self, peer: Arc<HlsPeer>) {
        self._hls_peer = Some(peer);
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
        if let Some(range) = segments.item_range((variant, segment_index)) {
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
            let seg_range = segments.item_range((variant, 0))?;
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
        if let Some(seg0_range) = segments.item_range((variant, 0)) {
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

    pub(super) fn push_segment_request(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        seek_epoch: u64,
    ) -> bool {
        self.coord
            .enqueue_segment_request(crate::coord::SegmentRequest {
                segment_index,
                variant,
                seek_epoch,
            })
    }

    /// Try to resolve and enqueue a segment request for `range_start`.
    /// Returns `true` if a segment could be resolved (even if the request
    /// was already pending). Returns `false` only when no resolution path
    /// can map the offset to a segment — a true metadata miss.
    pub(super) fn queue_segment_request_for_offset(
        &self,
        range_start: u64,
        seek_epoch: u64,
    ) -> bool {
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            self.push_segment_request(variant, segment_index, seek_epoch);
            return true;
        }
        let variant = self.resolve_current_variant();
        // Exact resolution: committed data or playlist metadata.
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            self.push_segment_request(variant, segment_index, seek_epoch);
            return true;
        }
        if let Some(segment_index) = self
            .playlist_state
            .find_segment_at_offset(variant, range_start)
        {
            self.push_segment_request(variant, segment_index, seek_epoch);
            return true;
        }
        // No resolution path found — size map not ready yet.
        // The caller's condvar loop will retry once the scheduler
        // commits data and notifies.
        false
    }

    pub(super) fn resolve_segment_for_offset(
        &self,
        range_start: u64,
        variant: VariantIndex,
    ) -> Option<SegmentIndex> {
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            return Some(segment_index);
        }
        self.playlist_state
            .find_segment_at_offset(variant, range_start)
    }

    pub(super) fn loaded_segment_ready(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> bool {
        let segments = self.segments.lock_sync();
        if !segments.is_visible(variant, segment_index) {
            return false;
        }
        segments
            .range_for(variant, segment_index)
            .is_some_and(|range| self.range_ready_from_segments(&segments, &range))
    }
}

/// Build an `HlsScheduler` + `HlsSource` pair from config.
///
/// `timeline` is the shared [`Timeline`] that the caller has already
/// handed to [`HlsPeer::new`]. Passing it in — instead of minting a new
/// one inside — guarantees the peer's `priority()` reads the same
/// `PLAYING` flag the audio FSM writes through the coord.
pub(crate) fn build_pair(
    backend: AssetStore<DecryptContext>,
    _track: kithara_stream::dl::PeerHandle,
    variants: &[crate::parsing::VariantStream],
    config: &crate::config::HlsConfig,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
    timeline: Timeline,
) -> (HlsScheduler, HlsSource) {
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

    let downloader = HlsScheduler {
        active_seek_epoch: 0,
        backend: backend.clone(),
        playlist_state: Arc::clone(&playlist_state),
        cursor: DownloadCursor::fill(0),
        force_init_for_seek: false,
        sent_init_for_variant: HashSet::new(),
        abr,
        coord: Arc::clone(&coord),
        segments: Arc::clone(&segments),
        bus: bus.clone(),
        look_ahead_segments,
        prefetch_count: config.download_batch_size.max(1),
        download_variant: initial_variant,
        filling_layout_gap: false,
        demand_throttle_until: None,
        announced_cached_count: HashMap::new(),
        in_flight_segments: HashSet::new(),
    };

    let source = HlsSource {
        coord,
        backend,
        segments,
        playlist_state,
        bus,
        variant_fence: None,
        _hls_peer: None,
        _peer_handle: None,
    };

    (downloader, source)
}
