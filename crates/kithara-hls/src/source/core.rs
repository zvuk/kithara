use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicUsize},
};

use kithara_assets::AssetStore;
use kithara_drm::DecryptContext;
use kithara_events::EventBus;
use kithara_platform::Mutex;
use kithara_test_utils::kithara;
use tracing::trace;

use crate::{
    coord::HlsCoord,
    ids::{SegmentIndex, VariantIndex},
    peer::HlsPeer,
    playlist::{PlaylistAccess, PlaylistState},
    source::HlsSegmentView,
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
    pub(crate) playlist_state: Arc<PlaylistState>,
    /// Shared with `HlsPeer::reader_segment` — `read_at` updates this
    /// cursor after each successful read so `HlsPeer::progress` can report
    /// `reader_playback_time` to the ABR controller.
    pub(crate) reader_segment: Arc<AtomicUsize>,
    /// Segment-aware metadata view passed into segment-by-segment
    /// decoders via `Source::as_segmented`. Aggregates `Arc` clones of
    /// `coord`, `playlist_state`, and `segments` so the decoder can query
    /// segment byte ranges and decode-time mappings without holding a
    /// reference back to `HlsSource`.
    pub(crate) segmented_view: Arc<HlsSegmentView>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
    pub(crate) backend: AssetStore<DecryptContext>,
    pub(crate) bus: EventBus,
    /// HLS peer. Cloned from the Downloader's registry; `Drop` tears down
    /// its [`HlsState`] so the stashed [`SegmentLoader`]'s internal
    /// `PeerHandle` clones release `PeerInner.cancel`, letting the
    /// registry remove its own `Arc<HlsPeer>`. Without this the whole
    /// peer graph leaks until the Downloader itself is dropped.
    pub(crate) _hls_peer: Option<Arc<HlsPeer>>,
    /// Peer handle. Dropped with this source, cancelling the peer.
    pub(crate) _peer_handle: Option<kithara_stream::dl::PeerHandle>,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<VariantIndex>,
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
    #[doc(hidden)]
    #[must_use]
    pub fn backend(&self) -> &AssetStore<DecryptContext> {
        &self.backend
    }

    #[doc(hidden)]
    #[must_use]
    pub fn coord(&self) -> &Arc<HlsCoord> {
        &self.coord
    }

    /// Override the variant fence — escape hatch for test scenarios that
    /// bypass the auto-detection that normally sets the fence on first read.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn set_variant_fence(&mut self, fence: Option<VariantIndex>) {
        self.variant_fence = fence;
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

    /// Whether a transition from `from_variant` to `to_variant` can keep
    /// the underlying stream state intact: true only if both variants
    /// share the same audio codec (the playlist's `variant_codec` fingerprint).
    /// A false answer means the reader must reset on the variant boundary.
    #[must_use]
    pub fn can_cross_variant_without_reset(
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
        if let Some(seg_ref) = segments.find_at_offset(pos).filter(|s| s.data.is_some()) {
            return Some(seg_ref.variant);
        }
        // Fallback: check the last committed segment across all variants
        if segments.max_end_offset() > 0
            && let Some(seg_ref) = segments
                .find_at_offset(segments.max_end_offset().saturating_sub(1))
                .filter(|s| s.data.is_some())
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
            .filter(|s| s.data.is_some())
            .or_else(|| {
                let max = segments.max_end_offset();
                (max > 0)
                    .then(|| {
                        segments
                            .find_at_offset(max.saturating_sub(1))
                            .filter(|s| s.data.is_some())
                    })
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

    pub(super) fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    fn find_segment_for_offset(
        &self,
        range_start: u64,
    ) -> Option<(VariantIndex, SegmentIndex, &'static str)> {
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            return Some((variant, segment_index, "layout_segment_for_offset"));
        }
        let variant = self.resolve_current_variant();
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            return Some((variant, segment_index, "committed_segment_for_offset"));
        }
        let segment_index = self
            .playlist_state
            .find_segment_at_offset(variant, range_start)?;
        Some((variant, segment_index, "playlist.find_segment_at_offset"))
    }

    #[kithara::probe(probe_return)]
    pub(super) fn init_segment_range_for_variant(
        &self,
        variant: VariantIndex,
    ) -> InitRangeResolution {
        let segments = self.segments.lock_sync();
        let Some(vs) = segments.variant_segments(variant) else {
            return InitRangeResolution::none(variant, 0, None, InitRangeOutcome::NoneNoVariant);
        };
        let committed_count = vs.iter().count() as u64;
        // Find first committed segment with init data
        let Some((seg_idx, seg_data)) = vs.iter().find(|(_, data)| data.init_len > 0) else {
            return InitRangeResolution::none(
                variant,
                committed_count,
                None,
                InitRangeOutcome::NoneNoCommittedInit,
            );
        };

        // Always return a range starting from offset 0 (segment 0).
        // During midstream ABR switches the downloader commits a later segment
        // first (e.g. segment 15 with init). Using that segment's byte offset
        // as base_offset makes the decoder see only a tail of the stream,
        // causing "unexpected end of file" on late-track seeks.
        // Returning 0-based range ensures the decoder sees the full stream.
        // The pipeline waits (source_is_ready_for_boundary) until segment 0
        // is actually downloaded before creating the decoder.
        if seg_idx == 0 {
            let Some(seg_range) = segments.item_range((variant, 0)) else {
                return InitRangeResolution::none(
                    variant,
                    committed_count,
                    Some(0),
                    InitRangeOutcome::NoneSeg0NoRange,
                );
            };
            if seg_data.init_url.is_none() {
                drop(segments);
                if let Some(metadata_range) = self.metadata_range_for_segment(variant, 0) {
                    return InitRangeResolution::ok(
                        variant,
                        committed_count,
                        Some(0),
                        InitRangeOutcome::OkMetadataFallback,
                        metadata_range,
                    );
                }
                return InitRangeResolution::ok(
                    variant,
                    committed_count,
                    Some(0),
                    InitRangeOutcome::OkSeg0,
                    seg_range.start..seg_range.end,
                );
            }
            return InitRangeResolution::ok(
                variant,
                committed_count,
                Some(0),
                InitRangeOutcome::OkSeg0,
                seg_range.start..seg_range.end,
            );
        }

        // Init-bearing segment is not segment 0 — use segment 0's range
        // (from byte map or metadata) so the decoder starts at offset 0.
        if let Some(seg0_range) = segments.item_range((variant, 0)) {
            return InitRangeResolution::ok(
                variant,
                committed_count,
                Some(seg_idx as u64),
                InitRangeOutcome::OkSeg0ViaRange,
                seg0_range,
            );
        }
        drop(segments);
        self.metadata_range_for_segment(variant, 0).map_or_else(
            || {
                InitRangeResolution::none(
                    variant,
                    committed_count,
                    Some(seg_idx as u64),
                    InitRangeOutcome::NoneFallback,
                )
            },
            |metadata_range| {
                InitRangeResolution::ok(
                    variant,
                    committed_count,
                    Some(seg_idx as u64),
                    InitRangeOutcome::OkMetadataFallback,
                    metadata_range,
                )
            },
        )
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

    pub(super) fn layout_segment_for_offset(
        &self,
        offset: u64,
    ) -> Option<(VariantIndex, SegmentIndex)> {
        self.segments
            .lock_sync()
            .segment_for_offset(offset, self.playlist_state.as_ref())
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

    /// Hand a resolved `(variant, segment_index)` pair off to the
    /// coord's demand slot for the given seek epoch. The probe records
    /// the (`seek_epoch`, `segment_index`, `offset`) tuple at the
    /// precise point the offset-driven path commits a request to the
    /// queue, so `probe_capture` consumers can match this against the
    /// scheduler's `fetch_cmd_emitted` for the same epoch+segment.
    #[kithara::probe(seek_epoch, segment_index, offset)]
    fn queue_resolved_segment_request(
        &self,
        seek_epoch: u64,
        segment_index: SegmentIndex,
        variant: VariantIndex,
        offset: u64,
    ) -> bool {
        let _ = offset;
        self.push_segment_request(variant, segment_index, seek_epoch)
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
        let Some((variant, segment_index, via)) = self.find_segment_for_offset(range_start) else {
            // No resolution path found — size map not ready yet.
            // The caller's condvar loop will retry once the scheduler
            // commits data and notifies.
            trace!(
                target: "hls_seek_diag",
                range_start,
                seek_epoch,
                "queue_segment_request_for_offset: no resolution (size map not ready)"
            );
            return false;
        };
        trace!(
            target: "hls_seek_diag",
            range_start,
            seek_epoch,
            variant,
            segment_index,
            via,
            "queue_segment_request_for_offset: enqueue"
        );
        self.queue_resolved_segment_request(seek_epoch, segment_index, variant, range_start)
    }

    /// Current variant for source operations (read, seek, demand).
    ///
    /// Always uses `layout_variant` — ABR only affects playback via
    /// `media_info()` → `format_change` detection → layout switch.
    pub(super) fn resolve_current_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
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

    /// Store the `Arc<HlsPeer>` so `Drop` can tear down its state and
    /// break the `Registry → HlsPeer → HlsState::loader → PeerHandle`
    /// reference cycle.
    pub(crate) fn set_hls_peer(&mut self, peer: Arc<HlsPeer>) {
        self._hls_peer = Some(peer);
    }

    /// Set the peer handle (called after the peer is activated).
    pub(crate) fn set_peer_handle(&mut self, handle: kithara_stream::dl::PeerHandle) {
        self._peer_handle = Some(handle);
    }
}

/// Outcome of [`HlsSource::init_segment_range_for_variant`].
///
/// Encodes which path resolved the byte range (or why it could not).
/// On the synthetic contract fixtures every variant's seg-0 is committed
/// upfront with an explicit `init_url`, so anything other than
/// [`InitRangeOutcome::OkSeg0`] in production logs is a code smell:
/// either the cache lost seg-0, the resolver raced against an ABR
/// mid-stream switch, or the byte map for seg-0 never committed.
///
/// Wire encoding (`u64`) is stable across versions and consumed by
/// `probe_capture` filters — do NOT renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitRangeOutcome {
    /// Segment 0 was committed and its byte map produced a valid range.
    OkSeg0 = 0,
    /// Segment 0 was committed without an explicit `init_url`; the
    /// range came from the playlist's metadata-derived byte offsets.
    OkMetadataFallback = 1,
    /// The init-bearing segment was not segment 0 (mid-stream ABR
    /// commit race); the returned range covers seg-0 instead.
    OkSeg0ViaRange = 2,
    /// The variant has no `VariantSegments` entry yet (variant index
    /// is out of range or the layout has not been populated).
    NoneNoVariant = 3,
    /// No committed segment carries init data — the resolver cannot
    /// produce a range until at least one segment with `init_len > 0`
    /// commits.
    NoneNoCommittedInit = 4,
    /// Seg-0 was found but its `item_range` lookup failed (corrupt
    /// byte map, transient race against `commit_segment`).
    NoneSeg0NoRange = 5,
    /// All fallbacks (committed seg-0 range, then metadata range)
    /// returned `None`. Resolver gave up.
    NoneFallback = 6,
}

impl kithara_test_utils::probes::IntoProbeArg for InitRangeOutcome {
    fn into_probe_arg(self) -> u64 {
        self as u64
    }
}

/// Result of [`HlsSource::init_segment_range_for_variant`]: the resolved
/// byte range plus the trace fields that explain *how* it was resolved.
///
/// Carries the wire payload of the `record_init_range_for_variant_outcome`
/// tracing probe; the `probe_return` mode picks the inherent
/// `record_probe` (from `derive(kithara::Probe)`) off the return value
/// automatically.
#[derive(Clone, kithara::Probe)]
pub(crate) struct InitRangeResolution {
    pub(crate) outcome: InitRangeOutcome,
    pub(crate) variant: u64,
    pub(crate) committed_count: u64,
    pub(crate) seg_idx_with_init: Option<u64>,
    #[probe(skip)]
    pub(crate) range: Option<Range<u64>>,
}

impl InitRangeResolution {
    pub(crate) fn ok(
        variant: VariantIndex,
        committed_count: u64,
        seg_idx_with_init: Option<u64>,
        outcome: InitRangeOutcome,
        range: Range<u64>,
    ) -> Self {
        Self {
            outcome,
            variant: variant as u64,
            committed_count,
            seg_idx_with_init,
            range: Some(range),
        }
    }

    pub(crate) fn none(
        variant: VariantIndex,
        committed_count: u64,
        seg_idx_with_init: Option<u64>,
        outcome: InitRangeOutcome,
    ) -> Self {
        Self {
            outcome,
            variant: variant as u64,
            committed_count,
            seg_idx_with_init,
            range: None,
        }
    }
}
