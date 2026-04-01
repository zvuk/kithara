//! HLS source: random-access reading from loaded HLS segments.
//!
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! Shared state with `HlsDownloader` via explicit `coord + segments + playlist_state`.

use std::{
    collections::HashSet,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara_abr::{AbrController, AbrOptions, Variant};
use kithara_assets::{AssetResourceState, ResourceKey};
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{
    Condvar, Mutex,
    thread::yield_now,
    time::{Duration, Instant},
    tokio,
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, StorageResource, WaitOutcome};
use kithara_stream::{
    DownloadCursor, MediaInfo, ReadOutcome, Source, SourcePhase, SourceSeekAnchor, StreamError,
    StreamResult, Timeline,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    downloader::{HlsDownloader, HlsIo},
    fetch::DefaultFetchManager,
    ids::{SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState},
    stream_index::{SegmentData, SegmentRef, StreamIndex},
};

/// HLS source: provides random-access reading from loaded segments.
///
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct HlsSource {
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) segments: Arc<Mutex<StreamIndex>>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<VariantIndex>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    pub(crate) _backend: Option<kithara_stream::Backend>,
    /// Dedup key for `wait_range_metadata_fallback` events. Encodes
    /// `(variant << 48) | offset` so the event fires at most once per
    /// unique (variant, offset) pair during a spin-wait loop.
    pub(crate) last_fallback_key: std::sync::atomic::AtomicU64,
}

const WAIT_RANGE_MAX_METADATA_MISS_SPINS: usize = 25;
/// Condvar sleep per spin in `wait_range`. Kept short so the audio worker
/// can round-robin between tracks without one slow source starving others.
/// Previous value 50ms caused audible glitches during multi-track mixing.
const WAIT_RANGE_SLEEP_MS: u64 = 2;
const WAIT_RANGE_HANG_TIMEOUT_FLOOR: Duration = Duration::from_secs(5);

/// Seek classification: whether the committed byte layout is preserved or reset.
enum SeekLayout {
    /// Same variant — keep `StreamIndex`, byte layout unchanged.
    Preserve,
    /// Different variant — reset `StreamIndex`, rebuild layout.
    Reset,
}

#[derive(Clone, Copy)]
enum ResolvedSegment {
    Committed(SegmentIndex),
    Layout(SegmentIndex),
    Fallback(SegmentIndex),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataMissReason {
    VariantOutOfRange,
    UnresolvedOffset,
}

impl MetadataMissReason {
    const fn label(self) -> Option<&'static str> {
        match self {
            Self::VariantOutOfRange => Some("variant_out_of_range"),
            Self::UnresolvedOffset => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemandRequestOutcome {
    Requested {
        queued: bool,
    },
    TransientGap,
    MetadataMiss {
        variant: VariantIndex,
        reason: MetadataMissReason,
    },
}

/// Snapshot of segment data needed for reading, copied out of the lock.
struct ReadSegment {
    variant: VariantIndex,
    segment_index: SegmentIndex,
    byte_offset: u64,
    init_len: u64,
    media_len: u64,
    init_url: Option<Url>,
    media_url: Url,
}

impl ReadSegment {
    /// Create from a `SegmentRef` (borrows from the lock — must copy).
    fn from_ref(seg_ref: &SegmentRef<'_>) -> Self {
        Self {
            variant: seg_ref.variant,
            segment_index: seg_ref.segment_index,
            byte_offset: seg_ref.byte_offset,
            init_len: seg_ref.data.init_len,
            media_len: seg_ref.data.media_len,
            init_url: seg_ref.data.init_url.clone(),
            media_url: seg_ref.data.media_url.clone(),
        }
    }

    fn end_offset(&self) -> u64 {
        self.byte_offset + self.init_len + self.media_len
    }
}

impl HlsSource {
    // construction / handles

    /// Set the backend (called after downloader is spawned).
    pub(crate) fn set_backend(&mut self, backend: kithara_stream::Backend) {
        self._backend = Some(backend);
    }

    // variant resolution

    /// Current variant for source operations (read, seek, demand).
    ///
    /// Always uses `layout_variant` — ABR only affects playback via
    /// `media_info()` → `format_change` detection → layout switch.
    fn resolve_current_variant(&self) -> VariantIndex {
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
    fn current_layout_variant(&self) -> Option<VariantIndex> {
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

    // segment lookup

    /// Returns `(variant, segment_index)` for the segment at the current reader position,
    /// falling back to the last committed segment if no segment covers the exact position.
    fn current_loaded_segment_key(&self) -> Option<(VariantIndex, SegmentIndex)> {
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

    fn current_segment_index(&self) -> Option<SegmentIndex> {
        self.current_loaded_segment_key()
            .map(|(_, seg_idx)| seg_idx)
    }

    fn layout_segment_for_offset(&self, offset: u64) -> Option<(VariantIndex, SegmentIndex)> {
        self.segments
            .lock_sync()
            .segment_for_offset(offset, self.playlist_state.as_ref())
    }

    fn byte_offset_for_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> Option<u64> {
        let segments = self.segments.lock_sync();
        if let Some(range) = <StreamIndex as kithara_stream::LayoutIndex>::item_range(
            &segments,
            (variant, segment_index),
        ) {
            return Some(range.start);
        }
        let layout_offset =
            segments.layout_offset_for_segment(segment_index, self.playlist_state.as_ref());
        drop(segments);
        self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .or(layout_offset)
    }

    fn metadata_range_for_segment(
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

    fn init_segment_range_for_variant(&self, variant: VariantIndex) -> Option<Range<u64>> {
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
            let seg_range =
                <StreamIndex as kithara_stream::LayoutIndex>::item_range(&segments, (variant, 0))?;
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
        if let Some(seg0_range) =
            <StreamIndex as kithara_stream::LayoutIndex>::item_range(&segments, (variant, 0))
        {
            return Some(seg0_range);
        }
        drop(segments);
        self.metadata_range_for_segment(variant, 0)
    }

    fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    fn fallback_segment_index_for_offset(
        &self,
        variant: VariantIndex,
        offset: u64,
    ) -> Option<SegmentIndex> {
        let num_segments = self.playlist_state.num_segments(variant)?;
        if num_segments == 0 {
            return None;
        }

        if let Some(total) = self.effective_total_bytes()
            && total > 0
        {
            if offset >= total {
                return None;
            }
            let estimate = ((u128::from(offset) * num_segments as u128) / u128::from(total))
                .min((num_segments - 1) as u128);
            return Some(estimate as usize);
        }

        let hinted = self.current_segment_index().unwrap_or(0);
        Some(hinted.min(num_segments - 1))
    }

    /// Check committed `StreamIndex` for the segment covering `range_start`.
    /// Returns `Some(segment_index)` if a request should be issued.
    fn committed_segment_for_offset(
        &self,
        range_start: u64,
        variant: VariantIndex,
    ) -> Option<SegmentIndex> {
        self.segments
            .lock_sync()
            .find_at_offset(range_start)
            .filter(|seg_ref| seg_ref.variant == variant)
            .map(|seg_ref| seg_ref.segment_index)
    }

    // phase / state

    /// Check if the read position is past the effective end of stream.
    fn is_past_eof(&self, segments: &StreamIndex, range: &Range<u64>) -> bool {
        let known_total = segments.effective_total(self.playlist_state.as_ref());
        let loaded_total = segments.max_end_offset();
        let effective_total = loaded_total.max(known_total);
        if effective_total == 0 || range.start < effective_total {
            return false;
        }

        true
    }

    fn is_transient_empty_eof(&self, segments: &StreamIndex) -> bool {
        self.coord.timeline().eof() && segments.effective_total(self.playlist_state.as_ref()) == 0
    }

    fn is_transient_demand_gap(&self) -> bool {
        let segments = self.segments.lock_sync();
        self.is_transient_empty_eof(&segments)
    }

    fn metadata_miss(
        &self,
        range_start: u64,
        seek_epoch: u64,
        current_variant: VariantIndex,
        metadata_miss_count: &mut usize,
        max_metadata_miss_spins: usize,
        reason: Option<&str>,
    ) -> Result<bool, String> {
        *metadata_miss_count = metadata_miss_count.saturating_add(1);
        self.bus.publish(HlsEvent::SeekMetadataMiss {
            seek_epoch,
            offset: range_start,
            variant: current_variant,
        });
        self.coord.reader_advanced.notify_one();

        if *metadata_miss_count < max_metadata_miss_spins {
            return Ok(false);
        }

        let error = reason.map_or_else(
            || {
                format!(
                    "seek metadata miss: offset={} variant={} epoch={} misses={}",
                    range_start, current_variant, seek_epoch, *metadata_miss_count
                )
            },
            |reason| {
                format!(
                    "seek metadata miss: offset={} variant={} epoch={} reason={}",
                    range_start, current_variant, seek_epoch, reason
                )
            },
        );
        self.bus.publish(HlsEvent::Error {
            error: error.clone(),
            recoverable: false,
        });
        Err(error)
    }

    fn log_resolved_demand_segment(
        &self,
        range_start: u64,
        seek_epoch: u64,
        current_variant: VariantIndex,
        resolved: ResolvedSegment,
    ) -> SegmentIndex {
        let segment_index = Self::segment_index(resolved);
        match resolved {
            ResolvedSegment::Committed(_) => {
                self.last_fallback_key.store(u64::MAX, Ordering::Relaxed);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: committed on-demand segment load"
                );
            }
            ResolvedSegment::Layout(_) => {
                self.last_fallback_key.store(u64::MAX, Ordering::Relaxed);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: layout on-demand segment load"
                );
            }
            ResolvedSegment::Fallback(_) => {
                let hint_index = self.current_segment_index().unwrap_or(0);
                trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: metadata miss fallback segment load"
                );
                let key = ((current_variant as u64) << 48) | (range_start & 0x0000_FFFF_FFFF_FFFF);
                let prev = self.last_fallback_key.swap(key, Ordering::Relaxed);
                if prev != key {
                    self.bus.publish(HlsEvent::Seek {
                        stage: "wait_range_metadata_fallback",
                        seek_epoch,
                        variant: current_variant,
                        offset: range_start,
                        from_segment_index: hint_index,
                        to_segment_index: segment_index,
                    });
                }
            }
        }
        segment_index
    }

    pub(crate) fn range_ready_from_segments(
        &self,
        segments: &StreamIndex,
        range: &Range<u64>,
    ) -> bool {
        let Some(seg_ref) = segments.find_at_offset(range.start) else {
            return false;
        };

        let seg_end = seg_ref.byte_offset + seg_ref.data.total_len();
        let range_end = range.end.min(seg_end);
        let local_start = range.start.saturating_sub(seg_ref.byte_offset);
        let local_end = range_end.saturating_sub(seg_ref.byte_offset);

        let init_end = seg_ref.data.init_len.min(local_end);
        if local_start < init_end {
            let Some(ref init_url) = seg_ref.data.init_url else {
                return false;
            };
            if !self.resource_covers_range(&ResourceKey::from_url(init_url), local_start..init_end)
            {
                return false;
            }
        }

        let media_start = local_start
            .max(seg_ref.data.init_len)
            .saturating_sub(seg_ref.data.init_len);
        let media_end = local_end.saturating_sub(seg_ref.data.init_len);
        if media_start < media_end
            && !self.resource_covers_range(
                &ResourceKey::from_url(&seg_ref.data.media_url),
                media_start..media_end,
            )
        {
            return false;
        }

        true
    }

    fn resource_covers_range(&self, key: &ResourceKey, range: Range<u64>) -> bool {
        match self.fetch.backend().resource_state(key) {
            Ok(AssetResourceState::Committed {
                final_len: Some(final_len),
            }) => range.end <= final_len,
            Ok(AssetResourceState::Active | AssetResourceState::Committed { .. }) => self
                .fetch
                .backend()
                .open_resource(key)
                .is_ok_and(|resource| resource.contains_range(range)),
            _ => false,
        }
    }

    // read

    /// Read from a loaded segment.
    ///
    /// Returns `Ok(None)` when the resource was evicted from the LRU cache
    /// between `wait_range` (metadata ready) and this read attempt.
    /// The caller should convert this to `ReadOutcome::Retry`.
    fn read_from_entry(
        &self,
        seg: &ReadSegment,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let local_offset = offset - seg.byte_offset;

        if local_offset < seg.init_len {
            let Some(ref init_url) = seg.init_url else {
                return Ok(Some(0));
            };

            let key = ResourceKey::from_url(init_url);
            let read_end = (local_offset + buf.len() as u64).min(seg.init_len);
            if !self.resource_covers_range(&key, local_offset..read_end) {
                return Ok(None);
            }
            let resource = self.fetch.backend().open_resource(&key)?;
            resource.wait_range(local_offset..read_end)?;

            #[expect(
                clippy::cast_possible_truncation,
                reason = "init segment fits in memory"
            )]
            let available = (seg.init_len - local_offset) as usize;
            let to_read = buf.len().min(available);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read])?;

            if bytes_from_init < buf.len() && seg.media_len > 0 {
                let remaining = &mut buf[bytes_from_init..];
                Ok(self
                    .read_media_segment_checked(seg, 0, remaining)?
                    .map(|n| bytes_from_init + n))
            } else {
                Ok(Some(bytes_from_init))
            }
        } else {
            let media_offset = local_offset - seg.init_len;
            self.read_media_segment_checked(seg, media_offset, buf)
        }
    }

    fn read_media_segment_checked(
        &self,
        seg: &ReadSegment,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let key = ResourceKey::from_url(&seg.media_url);
        let read_end = (media_offset + buf.len() as u64).min(seg.media_len);
        if !self.resource_covers_range(&key, media_offset..read_end) {
            return Ok(None);
        }
        let resource = self.fetch.backend().open_resource(&key)?;
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(Some(bytes_read))
    }

    // seek

    fn resolve_seek_anchor(&self, position: Duration) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        // Use current layout variant, NOT ABR target.
        // ABR must not affect seek resolution — variant switch happens
        // after seek completes, via format_change detection.
        let mut variant = self.segments.lock_sync().layout_variant();
        if variant >= variants {
            variant = 0;
        }

        let (segment_index, segment_start, segment_end) = self
            .playlist_state
            .find_seek_point_for_time(variant, position)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "seek point not found: variant={variant} target_ms={}",
                    position.as_millis()
                ))
            })?;

        let byte_offset = self
            .byte_offset_for_segment(variant, segment_index)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "seek offset not found: variant={variant} segment={}",
                    segment_index
                ))
            })?;

        #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
        let segment_index = segment_index as u32;
        Ok(SourceSeekAnchor {
            byte_offset,
            segment_start,
            segment_end: Some(segment_end),
            segment_index: Some(segment_index),
            variant_index: Some(variant),
        })
    }

    /// Classify a seek as Preserve or Reset.
    ///
    /// Since `resolve_seek_anchor` always uses `layout_variant` (not ABR target),
    /// seeks are always within the current layout → always Preserve.
    /// Variant switches happen via `format_change` detection, not during seek.
    fn classify_seek(&self, anchor: &SourceSeekAnchor) -> SeekLayout {
        let target_variant = anchor.variant_index.unwrap_or(0);
        match self.current_layout_variant() {
            Some(current) if current == target_variant => SeekLayout::Preserve,
            Some(current) if self.can_cross_variant_without_reset(current, target_variant) => {
                SeekLayout::Preserve
            }
            _ => SeekLayout::Reset,
        }
    }

    /// Apply the seek plan: update positions, optionally clear segments.
    fn apply_seek_plan(&mut self, anchor: &SourceSeekAnchor, layout: &SeekLayout) {
        let variant = anchor.variant_index.unwrap_or(0);
        let segment_index = anchor.segment_index.unwrap_or(0) as usize;
        let seek_epoch = self.coord.timeline().seek_epoch();
        let previous_hint = self.current_segment_index().unwrap_or(0);

        // Always: drain stale requests. The authoritative post-seek demand
        // is issued later from `commit_seek_landing(...)` once the decoder
        // tells us where it actually landed.
        self.coord.clear_segment_requests();

        match *layout {
            SeekLayout::Preserve => {
                // Keep segments — byte layout valid, decoder seeks in place.
                // layout_variant stays unchanged. ABR switch (if pending) is
                // handled after seek via format_change detection.
                trace!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Preserve — keeping StreamIndex"
                );
            }
            SeekLayout::Reset => {
                // Switch layout variant — decoder will be recreated.
                let mut segments = self.segments.lock_sync();
                segments.set_layout_variant(variant);
                // Sync expected sizes for the target variant so
                // rebuild_variant_byte_map can reserve correct gap offsets.
                if let Some(sizes) = self.playlist_state.segment_sizes(variant) {
                    segments.set_expected_sizes(variant, sizes);
                }
                drop(segments);
                self.coord.timeline().set_download_position(0);
                debug!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Reset — switched layout variant"
                );
            }
        }

        // Do not commit reader position here. The authoritative post-seek
        // byte position is whatever the decoder actually lands on after
        // `decoder.seek(...)` drives the underlying `Read + Seek` stream.
        self.coord.reader_advanced.notify_one();
        self.coord.condvar.notify_all();

        if previous_hint != segment_index {
            self.bus.publish(HlsEvent::Seek {
                stage: "seek_anchor_set_hint",
                seek_epoch,
                variant,
                offset: anchor.byte_offset,
                from_segment_index: previous_hint,
                to_segment_index: segment_index,
            });
        }

        trace!(
            seek_epoch,
            target_ms = ?anchor.segment_start,
            variant,
            segment_index,
            byte_offset = anchor.byte_offset,
            "seek_time_anchor: resolved seek anchor"
        );
    }

    // on-demand segment requests

    fn push_segment_request(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
        seek_epoch: u64,
    ) -> bool {
        self.coord.enqueue_segment_request(SegmentRequest {
            segment_index,
            variant,
            seek_epoch,
        })
    }

    fn resolve_demand_variant(&self) -> VariantIndex {
        self.resolve_current_variant()
    }

    fn queue_segment_request_for_offset(&self, range_start: u64, seek_epoch: u64) -> bool {
        // Always use layout_variant for demand — ABR is invisible here.
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            return self.push_segment_request(variant, segment_index, seek_epoch);
        }

        let variant = self.resolve_demand_variant();
        if let Some(resolved) = self.resolve_segment_for_offset(range_start, variant) {
            let segment_index = Self::segment_index(resolved);
            return self.push_segment_request(variant, segment_index, seek_epoch);
        }

        false
    }

    fn resolve_segment_for_offset(
        &self,
        range_start: u64,
        variant: VariantIndex,
    ) -> Option<ResolvedSegment> {
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            return Some(ResolvedSegment::Committed(segment_index));
        }

        // Try playlist metadata for layout-based resolution
        if let Some(segment_index) = self
            .playlist_state
            .find_segment_at_offset(variant, range_start)
        {
            return Some(ResolvedSegment::Layout(segment_index));
        }

        self.fallback_segment_index_for_offset(variant, range_start)
            .map(ResolvedSegment::Fallback)
    }

    fn segment_index(resolved: ResolvedSegment) -> SegmentIndex {
        match resolved {
            ResolvedSegment::Committed(segment_index)
            | ResolvedSegment::Layout(segment_index)
            | ResolvedSegment::Fallback(segment_index) => segment_index,
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "segments lock must be held for both is_segment_loaded and range_ready_from_segments"
    )]
    fn loaded_segment_ready(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        let segments = self.segments.lock_sync();
        if !segments.is_visible(variant, segment_index) {
            return false;
        }
        segments
            .range_for(variant, segment_index)
            .is_some_and(|range| self.range_ready_from_segments(&segments, &range))
    }

    #[expect(
        clippy::result_large_err,
        reason = "Stream source APIs use StreamResult<_, HlsError> consistently"
    )]
    fn request_on_demand_segment(&self, range_start: u64, seek_epoch: u64) -> DemandRequestOutcome {
        // Always use layout_variant for demand — ABR is invisible to source.
        if let Some((variant, segment_index)) = self.layout_segment_for_offset(range_start) {
            trace!(
                variant,
                segment_index,
                offset = range_start,
                "wait_range: layout on-demand segment load"
            );
            return DemandRequestOutcome::Requested {
                queued: self.push_segment_request(variant, segment_index, seek_epoch),
            };
        }

        let current_variant = self.resolve_demand_variant();
        if current_variant >= self.playlist_state.num_variants() {
            if self.is_transient_demand_gap() {
                self.coord.reader_advanced.notify_one();
                return DemandRequestOutcome::TransientGap;
            }

            return DemandRequestOutcome::MetadataMiss {
                variant: current_variant,
                reason: MetadataMissReason::VariantOutOfRange,
            };
        }

        if let Some(resolved) = self.resolve_segment_for_offset(range_start, current_variant) {
            let segment_index = Self::segment_index(resolved);
            self.log_resolved_demand_segment(range_start, seek_epoch, current_variant, resolved);
            return DemandRequestOutcome::Requested {
                queued: self.push_segment_request(current_variant, segment_index, seek_epoch),
            };
        }

        debug!(
            offset = range_start,
            variant = current_variant,
            seek_epoch,
            "wait_range: no metadata to find segment"
        );
        if self.is_transient_demand_gap() {
            self.coord.reader_advanced.notify_one();
            return DemandRequestOutcome::TransientGap;
        }

        DemandRequestOutcome::MetadataMiss {
            variant: current_variant,
            reason: MetadataMissReason::UnresolvedOffset,
        }
    }
}

fn wait_range_hang_timeout(timeout: Duration) -> Duration {
    timeout.max(WAIT_RANGE_HANG_TIMEOUT_FLOOR)
}

impl Source for HlsSource {
    type Error = HlsError;
    type Topology = Arc<PlaylistState>;
    type Layout = Arc<Mutex<StreamIndex>>;
    type Coord = Arc<HlsCoord>;
    type Demand = SegmentRequest;

    fn topology(&self) -> &Self::Topology {
        &self.playlist_state
    }

    fn layout(&self) -> &Self::Layout {
        &self.segments
    }

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock must be held for condvar wait"
    )]
    #[kithara_hang_detector::hang_watchdog(timeout = wait_range_hang_timeout(timeout))]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, HlsError> {
        let mut metadata_miss_count: usize = 0;
        let mut wait_seek_epoch: Option<u64> = None;
        let started_at = Instant::now();

        loop {
            let mut segments = self.segments.lock_sync();
            let seek_epoch = self.coord.timeline().seek_epoch();
            match wait_seek_epoch {
                Some(epoch) if epoch != seek_epoch => {
                    metadata_miss_count = 0;
                    wait_seek_epoch = Some(seek_epoch);
                    if !self.coord.timeline().is_seek_pending() {
                        return Ok(WaitOutcome::Interrupted);
                    }
                }
                None => wait_seek_epoch = Some(seek_epoch),
                _ => {}
            }

            // Compute phase inline — priority order matches SourcePhase::classify.
            let cancelled = self.coord.cancel.is_cancelled();
            let stopped = self.coord.stopped.load(Ordering::Acquire);
            let range_ready = self.range_ready_from_segments(&segments, &range);
            let seeking = self.coord.timeline().is_flushing();
            let past_eof = self.is_past_eof(&segments, &range);
            let waiting_demand = self.coord.has_pending_segment_request(seek_epoch);
            let waiting_metadata = metadata_miss_count > 0 && !waiting_demand;

            if range_ready {
                hang_reset!();
            }

            // Priority dispatch (highest wins).
            if cancelled || (stopped && !range_ready) {
                return if stopped && past_eof {
                    Ok(WaitOutcome::Eof)
                } else {
                    Err(StreamError::Source(HlsError::Cancelled))
                };
            }
            if range_ready {
                self.coord
                    .had_midstream_switch
                    .store(false, Ordering::Release);
                return Ok(WaitOutcome::Ready);
            }
            if seeking {
                return Ok(WaitOutcome::Interrupted);
            }
            if past_eof {
                debug!(
                    range_start = range.start,
                    max_end = segments.max_end_offset(),
                    known_total = segments.effective_total(self.playlist_state.as_ref()),
                    num_committed = segments.num_committed(),
                    "wait_range: EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            // Waiting sub-states — log and continue spinning.
            let phase = if waiting_metadata {
                SourcePhase::WaitingMetadata
            } else if waiting_demand {
                SourcePhase::WaitingDemand
            } else {
                SourcePhase::Waiting
            };
            trace!(
                range_start = range.start,
                range_end = range.end,
                ?phase,
                "wait_range: spinning"
            );

            drop(segments);

            // Re-issue the demand on each spin so a stale pending request for
            // the same epoch gets replaced by the segment that owns this range.
            // During a stale EOF + zero-total reset window, demand resolution
            // can legitimately observe an empty layout before fresh metadata
            // arrives; treat that as a transient gap instead of a terminal miss.
            match self.request_on_demand_segment(range.start, seek_epoch) {
                DemandRequestOutcome::Requested { .. } | DemandRequestOutcome::TransientGap => {
                    metadata_miss_count = 0;
                }
                DemandRequestOutcome::MetadataMiss { variant, reason } => {
                    self.metadata_miss(
                        range.start,
                        seek_epoch,
                        variant,
                        &mut metadata_miss_count,
                        WAIT_RANGE_MAX_METADATA_MISS_SPINS,
                        reason.label(),
                    )
                    .map_err(|error| StreamError::Source(HlsError::SegmentNotFound(error)))?;
                }
            }

            // Honour the overall time budget.  Placed after decision logic
            // and on-demand requests so that errors (e.g. SegmentNotFound)
            // always take priority over the timeout fallback.
            if started_at.elapsed() > timeout {
                return Err(StreamError::Source(HlsError::Timeout(format!(
                    "wait_range budget exceeded: range={}..{} elapsed={:?} timeout={timeout:?}",
                    range.start,
                    range.end,
                    started_at.elapsed(),
                ))));
            }

            segments = self.segments.lock_sync();

            hang_tick!();
            yield_now();
            let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
            let (_segments, _wait_result) =
                self.coord.condvar.wait_sync_timeout(segments, deadline);

            if self.coord.timeline().is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "segments guard used across multiple checks"
    )]
    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let segments = self.segments.lock_sync();
        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }
        if self.range_ready_from_segments(&segments, &range) {
            return SourcePhase::Ready;
        }
        // ABR variant transition stall: decoder reads from layout_variant, but
        // the downloader switched to a different variant. If range_start is past
        // all committed data for layout_variant, data will never arrive.
        // Report Ready so read_at can detect VariantChange.
        if range.start >= segments.max_end_offset() && segments.max_end_offset() > 0 {
            let abr_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
            if abr_variant != segments.layout_variant() {
                return SourcePhase::Ready;
            }
        }
        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if self.is_past_eof(&segments, &range) {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.timeline().byte_position();

        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }

        let (segment_ready, past_eof) = {
            let segments = self.segments.lock_sync();
            let ready = segments.find_at_offset(pos).is_some_and(|seg_ref| {
                let seg_range = seg_ref.byte_offset..seg_ref.byte_offset + seg_ref.data.total_len();
                self.range_ready_from_segments(&segments, &seg_range)
            });
            let eof = self.is_past_eof(&segments, &(pos..pos.saturating_add(1)));
            drop(segments);
            (ready, eof)
        };

        if segment_ready {
            return SourcePhase::Ready;
        }
        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if past_eof {
            return SourcePhase::Eof;
        }

        SourcePhase::Waiting
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, HlsError> {
        let read_epoch = self.coord.timeline().seek_epoch();
        let (seg, effective_total) = {
            let segments = self.segments.lock_sync();
            let found = segments
                .visible_segment_at(offset)
                .map(|r| ReadSegment::from_ref(&r));
            (
                found,
                segments.effective_total(self.playlist_state.as_ref()),
            )
        };

        let Some(seg) = seg else {
            if effective_total > 0 && offset >= effective_total {
                return Ok(ReadOutcome::Data(0));
            }

            // ABR variant transition stall detection:
            // Decoder reads from layout_variant's byte map, but data will never
            // arrive if the downloader switched to a different variant.
            // Signal VariantChange so the FSM recreates the decoder.
            let layout_variant = self.segments.lock_sync().layout_variant();
            let abr_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
            if abr_variant != layout_variant {
                trace!(
                    offset,
                    layout_variant,
                    abr_variant,
                    "read_at: ABR variant stall — signaling VariantChange"
                );
                return Ok(ReadOutcome::VariantChange);
            }

            self.queue_segment_request_for_offset(offset, read_epoch);
            return Ok(ReadOutcome::Retry);
        };

        let previous_hint = self.current_segment_index().unwrap_or(seg.segment_index);
        if seg.segment_index < previous_hint {
            self.bus.publish(HlsEvent::Seek {
                stage: "read_at_moved_segment_backward",
                seek_epoch: self.coord.timeline().seek_epoch(),
                variant: seg.variant,
                offset,
                from_segment_index: previous_hint,
                to_segment_index: seg.segment_index,
            });
        }

        // Variant fence: auto-detect on first read, block cross-variant reads.
        if self.variant_fence.is_none() {
            self.variant_fence = Some(seg.variant);
        }
        if let Some(fence) = self.variant_fence
            && seg.variant != fence
        {
            if self.can_cross_variant_without_reset(fence, seg.variant) {
                self.variant_fence = Some(seg.variant);
            } else {
                return Ok(ReadOutcome::VariantChange);
            }
        }

        let Some(bytes) = self
            .read_from_entry(&seg, offset, buf)
            .map_err(StreamError::Source)?
        else {
            // Resource evicted. Push an on-demand request so the downloader
            // re-fetches this segment even when it's at the tail (Idle state).
            let seek_epoch = self.coord.timeline().seek_epoch();
            self.push_segment_request(seg.variant, seg.segment_index, seek_epoch);
            return Ok(ReadOutcome::Retry);
        };

        if bytes > 0 {
            if self.coord.timeline().seek_epoch() != read_epoch {
                return Ok(ReadOutcome::Retry);
            }
            let new_pos = offset.saturating_add(bytes as u64);
            if seg.segment_index != previous_hint {
                self.coord.reader_advanced.notify_one();
            }

            let total = self.segments.lock_sync().max_end_offset();
            self.bus.publish(HlsEvent::ByteProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(ReadOutcome::Data(bytes))
    }

    fn len(&self) -> Option<u64> {
        self.effective_total_bytes()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
        let reader_variant = self.current_loaded_segment_key().map(|(v, _)| v);
        let has_hinted_variant = self
            .segments
            .lock_sync()
            .variant_segments(hinted_variant)
            .is_some_and(|vs| !vs.is_empty());
        let variant = match reader_variant {
            Some(reader) if reader == hinted_variant => reader,
            Some(_reader) if self.variant_fence.is_some() && has_hinted_variant => hinted_variant,
            Some(reader) => reader,
            None if has_hinted_variant => hinted_variant,
            None if hinted_variant < self.playlist_state.num_variants() => hinted_variant,
            None => return None,
        };
        let codec = self.playlist_state.variant_codec(variant);
        let container = self.playlist_state.variant_container(variant);
        #[expect(clippy::cast_possible_truncation, reason = "variant index fits in u32")]
        Some(MediaInfo::new(codec, container).with_variant_index(variant as u32))
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        let (variant, seg_idx) = self.current_loaded_segment_key()?;
        let segments = self.segments.lock_sync();
        <StreamIndex as kithara_stream::LayoutIndex>::item_range(&segments, (variant, seg_idx))
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let current_variant = self.coord.abr_variant_index.load(Ordering::Acquire);

        // Do NOT change layout_variant here — this method is called from
        // detect_format_change() while the old decoder is still reading.
        // Switching layout now would make the old decoder read from the
        // wrong variant's byte map, corrupting Symphonia's moof table.
        //
        // Layout change happens later in commit_variant_layout(), called
        // from step_recreating_decoder() right before building the new
        // decoder.

        if let Some(range) = self.init_segment_range_for_variant(current_variant) {
            return Some(range);
        }

        let fallback_variant = {
            let segments = self.segments.lock_sync();
            let reader_offset = self.coord.timeline().byte_position();
            segments
                .find_at_offset(reader_offset)
                .map(|seg_ref| seg_ref.variant)
                .or_else(|| {
                    // Fallback: last committed segment
                    let max = segments.max_end_offset();
                    if max > 0 {
                        segments
                            .find_at_offset(max.saturating_sub(1))
                            .map(|seg_ref| seg_ref.variant)
                    } else {
                        None
                    }
                })
        };
        if let Some(fallback_variant) = fallback_variant
            && let Some(range) = self.init_segment_range_for_variant(fallback_variant)
        {
            return Some(range);
        }

        // After seek flush, no segments may be loaded yet.
        // Fall back to metadata for the first logical segment in the
        // current applied layout rather than variant segment 0.
        let layout_floor = self.segments.lock_sync().layout_floor_segment();
        if let Some((variant, segment_index)) = layout_floor {
            return self.metadata_range_for_segment(variant, segment_index);
        }

        if current_variant >= self.playlist_state.num_variants() {
            return None;
        }
        self.metadata_range_for_segment(current_variant, 0)
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }

    fn commit_variant_layout(&mut self) {
        let target = self.coord.abr_variant_index.load(Ordering::Acquire);
        let mut segments = self.segments.lock_sync();
        if segments.layout_variant() != target {
            segments.set_layout_variant(target);
            if let Some(sizes) = self.playlist_state.segment_sizes(target) {
                segments.set_expected_sizes(target, sizes);
            }
        }
    }

    fn notify_waiting(&self) {
        self.coord.condvar.notify_all();
        self.coord.reader_advanced.notify_one();
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let coord = Arc::clone(&self.coord);
        Some(Box::new(move || {
            coord.condvar.notify_all();
            coord.reader_advanced.notify_one();
        }))
    }

    #[kithara_hang_detector::hang_watchdog]
    fn set_seek_epoch(&mut self, _seek_epoch: u64) {
        // Non-destructive: does NOT clear StreamIndex or download_position.
        // seek_time_anchor → classify_seek → apply_seek_plan handles that
        // conditionally based on SeekLayout (Preserve vs Reset).
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);

        self.coord.clear_segment_requests();

        self.coord.reader_advanced.notify_one();
        self.coord.condvar.notify_all();
    }

    fn seek_time_anchor(
        &mut self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>, HlsError> {
        let anchor = self
            .resolve_seek_anchor(position)
            .map_err(StreamError::Source)?;
        let layout = self.classify_seek(&anchor);
        self.apply_seek_plan(&anchor, &layout);
        Ok(Some(anchor))
    }

    fn commit_seek_landing(&mut self, anchor: Option<SourceSeekAnchor>) {
        let seek_epoch = self.coord.timeline().seek_epoch();
        let landed_offset = self.coord.timeline().byte_position();
        let fallback_variant = anchor
            .and_then(|resolved| resolved.variant_index)
            .unwrap_or_else(|| self.resolve_current_variant());
        let landed_segment = self
            .layout_segment_for_offset(landed_offset)
            .filter(|&(variant, segment_index)| {
                !anchor.is_some_and(|resolved| {
                    let Some(anchor_variant) = resolved.variant_index else {
                        return false;
                    };
                    let Some(anchor_segment) = resolved.segment_index.map(|index| index as usize)
                    else {
                        return false;
                    };
                    landed_offset < resolved.byte_offset
                        && variant == anchor_variant
                        && segment_index >= anchor_segment
                })
            })
            .or_else(|| {
                self.resolve_segment_for_offset(landed_offset, fallback_variant)
                    .map(|resolved| (fallback_variant, Self::segment_index(resolved)))
            });
        // `apply_seek_plan(...)` already established the authoritative layout
        // for this seek. Rewriting `variant_map` again at the landed offset
        // collapses mixed auto-switch layouts during replay from the prefix.
        let Some((variant, segment_index)) = landed_segment else {
            debug!(
                seek_epoch,
                variant = fallback_variant,
                landed_offset,
                "commit_seek_landing: could not resolve landed segment"
            );
            self.coord.condvar.notify_all();
            self.coord.reader_advanced.notify_one();
            return;
        };
        self.variant_fence = Some(variant);
        let segment_ready = self.loaded_segment_ready(variant, segment_index);

        let previous_hint = self.current_segment_index().unwrap_or(segment_index);
        self.coord.clear_segment_requests();
        if !segment_ready {
            self.coord.enqueue_segment_request(SegmentRequest {
                segment_index,
                variant,
                seek_epoch,
            });
        }
        self.coord.condvar.notify_all();

        if previous_hint != segment_index {
            self.bus.publish(HlsEvent::Seek {
                stage: "seek_landing_set_segment",
                seek_epoch,
                variant,
                offset: landed_offset,
                from_segment_index: previous_hint,
                to_segment_index: segment_index,
            });
        }

        trace!(
            seek_epoch,
            variant,
            segment_index,
            landed_offset,
            queued = !segment_ready,
            "commit_seek_landing: reconciled landed segment"
        );
    }

    fn demand_range(&self, range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let seek_epoch = self.coord.timeline().seek_epoch();
        self.queue_segment_request_for_offset(range.start, seek_epoch);
    }
}

/// Build an `HlsDownloader` + `HlsSource` pair from config.
pub(crate) fn build_pair(
    fetch: Arc<DefaultFetchManager>,
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
    let look_ahead_segments = if fetch.backend().is_ephemeral() {
        const SLOTS_PER_SEGMENT: usize = 2;
        let cache_cap = config.store.effective_cache_capacity().get();
        Some((cache_cap.saturating_sub(SLOTS_PER_SEGMENT) / SLOTS_PER_SEGMENT).max(1))
    } else {
        None
    };

    let downloader = HlsDownloader {
        active_seek_epoch: 0,
        io: HlsIo::new(Arc::clone(&fetch)),
        fetch: Arc::clone(&fetch),
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
        fetch,
        segments,
        playlist_state,
        bus,
        variant_fence: None,
        _backend: None,
        last_fallback_key: std::sync::atomic::AtomicU64::new(u64::MAX),
    };

    (downloader, source)
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, path::Path, thread, time::Duration as StdDuration};

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio::time::{Duration as TokioDuration, timeout};
    use url::Url;

    use super::*;
    use crate::{
        config::HlsConfig,
        fetch::{DefaultFetchManager, FetchManager},
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
        stream_index::SegmentData,
    };

    fn test_fetch_manager(cancel: CancellationToken) -> Arc<DefaultFetchManager> {
        let noop_drm: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });
        let backend = AssetStoreBuilder::new()
            .ephemeral(true)
            .cancel(cancel.clone())
            .process_fn(noop_drm)
            .build();
        let net = HttpClient::new(NetOptions::default());
        Arc::new(FetchManager::new(backend, net, cancel))
    }

    fn test_disk_fetch_manager(
        cancel: CancellationToken,
        root_dir: &Path,
    ) -> Arc<DefaultFetchManager> {
        let noop_drm: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });
        let backend = AssetStoreBuilder::new()
            .root_dir(root_dir)
            .asset_root(Some("source-disk-test"))
            .cache_capacity(NonZeroUsize::new(1).expect("non-zero"))
            .cancel(cancel.clone())
            .process_fn(noop_drm)
            .build();
        let net = HttpClient::new(NetOptions::default());
        Arc::new(FetchManager::new(backend, net, cancel))
    }

    fn parsed_variants(count: usize) -> Vec<VariantStream> {
        (0..count)
            .map(|index| VariantStream {
                id: VariantId(index),
                uri: format!("v{index}.m3u8"),
                bandwidth: Some(128_000),
                name: None,
                codec: None,
            })
            .collect()
    }

    fn make_variant_state_with_segments(id: usize, segments: usize) -> VariantState {
        let base = Url::parse("https://example.com/").expect("valid base URL");
        VariantState {
            id,
            uri: base
                .join(&format!("v{id}.m3u8"))
                .expect("valid playlist URL"),
            bandwidth: Some(128_000),
            codec: None,
            container: None,
            init_url: None,
            segments: (0..segments)
                .map(|index| SegmentState {
                    index,
                    url: base
                        .join(&format!("seg-{id}-{index}.m4s"))
                        .expect("valid segment URL"),
                    duration: Duration::from_secs(4),
                    key: None,
                })
                .collect(),
            size_map: None,
        }
    }

    fn make_variant_state(id: usize) -> VariantState {
        make_variant_state_with_segments(id, 1)
    }

    fn build_test_source_with_segments(
        num_variants: usize,
        segments_per_variant: usize,
    ) -> HlsSource {
        let cancel = CancellationToken::new();
        let variants: Vec<VariantState> = (0..num_variants)
            .map(|index| make_variant_state_with_segments(index, segments_per_variant))
            .collect();
        let playlist_state = Arc::new(PlaylistState::new(variants));
        let parsed = parsed_variants(num_variants);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));
        source
    }

    fn build_test_source(num_variants: usize) -> HlsSource {
        build_test_source_with_segments(num_variants, 1)
    }

    fn build_source_with_size_map(segment_sizes: &[u64]) -> HlsSource {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            segment_sizes.len(),
        )]));
        let mut total = 0;
        let offsets: Vec<u64> = segment_sizes
            .iter()
            .map(|size| {
                let offset = total;
                total += size;
                offset
            })
            .collect();
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                segment_sizes: segment_sizes.to_vec(),
                offsets,
                total,
            },
        );
        let parsed = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));
        source
    }

    fn set_variant_size_map(state: &PlaylistState, variant: usize, segment_sizes: &[u64]) {
        let mut total = 0;
        let offsets: Vec<u64> = segment_sizes
            .iter()
            .map(|size| {
                let offset = total;
                total += size;
                offset
            })
            .collect();
        state.set_size_map(
            variant,
            VariantSizeMap {
                init_size: 0,
                segment_sizes: segment_sizes.to_vec(),
                offsets,
                total,
            },
        );
    }

    fn push_segment(
        segments: &Arc<Mutex<StreamIndex>>,
        variant: usize,
        index: usize,
        _offset: u64,
        len: u64,
    ) {
        let data = SegmentData {
            init_len: 0,
            media_len: len,
            init_url: None,
            media_url: Url::parse(&format!("https://example.com/seg-{variant}-{index}.m4s"))
                .expect("valid segment URL"),
        };
        segments.lock_sync().commit_segment(variant, index, data);
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "test helper, segment index fits u32"
    )]
    fn make_anchor(variant: usize, segment: usize, offset: u64) -> SourceSeekAnchor {
        SourceSeekAnchor {
            byte_offset: offset,
            segment_start: Duration::ZERO,
            segment_end: None,
            variant_index: Some(variant),
            segment_index: Some(segment as u32),
        }
    }

    #[kithara::test]
    fn same_variant_seek_preserves_segments() {
        let source = build_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);
        push_segment(&source.segments, 0, 1, 100, 100);

        let anchor = make_anchor(0, 0, 0);
        let layout = source.classify_seek(&anchor);
        assert!(matches!(layout, SeekLayout::Preserve));
    }

    #[kithara::test]
    fn seek_resolves_in_layout_variant_not_abr_target() {
        let source = build_test_source_with_segments(2, 4);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
        set_variant_size_map(source.playlist_state.as_ref(), 1, &[500, 500, 500, 500]);
        push_segment(&source.segments, 0, 0, 0, 100);

        // ABR wants variant 1, but layout is variant 0
        source.coord.abr_variant_index.store(1, Ordering::Release);

        let anchor = source
            .resolve_seek_anchor(Duration::from_millis(500))
            .expect("anchor resolution");

        // Anchor must use layout variant (0), NOT ABR target (1)
        assert_eq!(
            anchor.variant_index,
            Some(0),
            "seek must resolve in layout_variant, ignoring ABR target"
        );
    }

    #[kithara::test]
    fn seek_does_not_switch_layout_variant() {
        let mut source = build_test_source_with_segments(2, 4);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
        push_segment(&source.segments, 0, 0, 0, 100);

        source.coord.abr_variant_index.store(1, Ordering::Release);

        let anchor = make_anchor(0, 0, 0);
        let layout = source.classify_seek(&anchor);
        source.apply_seek_plan(&anchor, &layout);

        assert_eq!(
            source.segments.lock_sync().layout_variant(),
            0,
            "seek must not switch layout_variant — ABR switch happens via format_change"
        );
    }

    #[kithara::test]
    fn seek_within_layout_variant_preserves_segments() {
        let source = build_test_source_with_segments(2, 3);
        // Commit segments to layout variant (0)
        push_segment(&source.segments, 0, 0, 0, 100);
        push_segment(&source.segments, 0, 1, 100, 100);
        push_segment(&source.segments, 0, 2, 200, 100);

        source.coord.timeline().set_byte_position(250);

        // Seek to variant 0 (same as layout_variant) → Preserve
        let anchor = make_anchor(0, 0, 0);
        let layout = source.classify_seek(&anchor);
        assert!(
            matches!(layout, SeekLayout::Preserve),
            "seek within the layout variant must preserve StreamIndex"
        );
    }

    #[kithara::test]
    fn commit_seek_landing_keeps_switched_tail_in_mixed_layout() {
        let mut source = build_test_source_with_segments(2, 2);
        push_segment(&source.segments, 0, 0, 0, 100);
        {
            let mut segments = source.segments.lock_sync();
            segments.set_layout_variant(1);
        }
        push_segment(&source.segments, 1, 1, 100, 100);
        source.coord.timeline().set_byte_position(0);

        source.commit_seek_landing(Some(make_anchor(0, 0, 0)));

        let segments = source.segments.lock_sync();
        assert_eq!(
            segments.layout_variant(),
            1,
            "landing on the prefix must not rewrite the switched tail back to variant 0"
        );
    }

    #[kithara::test]
    fn resolve_current_variant_uses_layout_variant() {
        let source = build_test_source(2);
        source.coord.abr_variant_index.store(1, Ordering::Release);

        // resolve_current_variant uses layout_variant, not ABR
        assert_eq!(
            source.resolve_current_variant(),
            0,
            "current variant must come from layout_variant, not ABR atomic"
        );
    }

    #[kithara::test]
    fn seek_anchor_uses_layout_variant_not_abr_target() {
        let mut source = build_test_source_with_segments(2, 4);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
        // ABR wants variant 1, but seek must use layout_variant (0)
        source.coord.abr_variant_index.store(1, Ordering::Release);

        let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
            .expect("seek anchor resolution should not error")
            .expect("HLS source should resolve an anchor");

        assert_eq!(
            anchor.variant_index,
            Some(0),
            "seek must resolve in layout_variant (0), not ABR target (1)"
        );
        assert_eq!(anchor.segment_index, Some(2));
        assert_eq!(
            anchor.byte_offset, 200,
            "seek offset must be in layout variant's byte space"
        );
    }

    #[kithara::test]
    fn abr_does_not_affect_any_seek_state() {
        let mut source = build_test_source_with_segments(2, 4);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
        set_variant_size_map(source.playlist_state.as_ref(), 1, &[500, 500, 500, 500]);
        push_segment(&source.segments, 0, 0, 0, 100);
        push_segment(&source.segments, 0, 1, 100, 100);

        // ABR switches to variant 1 mid-stream
        source.coord.abr_variant_index.store(1, Ordering::Release);
        source
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);

        // Seek resolves in layout_variant (0), ignoring ABR
        let anchor = source
            .resolve_seek_anchor(Duration::from_millis(4_000))
            .expect("anchor");
        assert_eq!(anchor.variant_index, Some(0), "anchor ignores ABR");

        // classify_seek returns Preserve (same layout variant)
        let layout = source.classify_seek(&anchor);
        assert!(
            matches!(layout, SeekLayout::Preserve),
            "seek within layout_variant is always Preserve"
        );

        // apply_seek_plan does not change layout_variant
        source.apply_seek_plan(&anchor, &layout);
        assert_eq!(
            source.segments.lock_sync().layout_variant(),
            0,
            "layout_variant unchanged after seek"
        );

        // demand routing uses layout_variant (0), not ABR (1)
        assert_eq!(
            source.resolve_current_variant(),
            0,
            "demand variant is layout_variant during seek"
        );
    }

    #[kithara::test]
    fn commit_seek_landing_uses_layout_variant_for_invalidated_segment() {
        let mut source = build_test_source(2);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100]);
        set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100]);

        // Layout variant = 0, commit and evict segment 0
        push_segment(&source.segments, 0, 0, 0, 100);
        push_segment(&source.segments, 0, 1, 100, 100);

        let evicted = ResourceKey::from_url(
            &Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
        );
        {
            let mut segments = source.segments.lock_sync();
            assert!(segments.remove_resource(&evicted));
        }

        // ABR wants variant 1, but layout is still variant 0
        source.coord.abr_variant_index.store(1, Ordering::Release);
        source.coord.timeline().set_byte_position(0);

        source.commit_seek_landing(Some(make_anchor(0, 0, 0)));

        // Recovery must target layout variant (0), not ABR target (1)
        assert_eq!(
            source.variant_fence,
            Some(0),
            "seek landing must fence reads to the layout variant, not the ABR target"
        );
        assert_eq!(
            source.coord.take_segment_request(),
            Some(SegmentRequest {
                variant: 0,
                segment_index: 0,
                seek_epoch: 0,
            }),
            "seek landing recovery must target the layout variant that owns the missing segment"
        );
    }

    #[kithara::test]
    fn commit_seek_landing_uses_anchor_variant_metadata_when_reset_truncates_prefix() {
        let mut source = build_test_source_with_segments(2, 4);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100, 100, 100]);
        set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100, 100, 100]);
        source.coord.abr_variant_index.store(1, Ordering::Release);

        let anchor = make_anchor(1, 2, 200);
        source.apply_seek_plan(&anchor, &SeekLayout::Reset);
        source.coord.timeline().set_byte_position(150);

        source.commit_seek_landing(Some(anchor));

        assert_eq!(
            source.coord.take_segment_request(),
            Some(SegmentRequest {
                variant: 1,
                segment_index: 1,
                seek_epoch: 0,
            }),
            "decoder landing before the anchor must resolve through target-variant metadata, not the truncated reset layout"
        );
    }

    #[kithara::test(tokio)]
    async fn on_demand_request_notifies_downloader_once() {
        let source = build_test_source(1);
        let wake = source.coord.reader_advanced.notified();

        let outcome = source.request_on_demand_segment(0, 7);
        assert_eq!(outcome, DemandRequestOutcome::Requested { queued: true });

        let req = source
            .coord
            .take_segment_request()
            .expect("request must be queued");
        assert_eq!(req.variant, 0);
        assert_eq!(req.segment_index, 0);
        assert_eq!(req.seek_epoch, 7);

        timeout(TokioDuration::from_millis(10), wake)
            .await
            .expect("initial on-demand request must wake downloader");
    }

    #[kithara::test]
    fn on_demand_request_returns_false_when_dedupe_suppresses_enqueue() {
        let source = build_test_source(1);
        let request = SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: 7,
        };
        source.coord.requeue_segment_request(request);

        let outcome = source.request_on_demand_segment(0, 7);

        assert!(
            matches!(outcome, DemandRequestOutcome::Requested { queued: false }),
            "request_on_demand_segment must report dedupe when enqueue is suppressed"
        );
        assert_eq!(
            source.coord.take_segment_request(),
            Some(request),
            "dedupe must keep the original pending request instead of enqueuing a duplicate"
        );
    }

    #[kithara::test]
    fn queue_segment_request_uses_layout_variant_for_invalidated_segment() {
        let source = build_test_source(2);
        set_variant_size_map(source.playlist_state.as_ref(), 0, &[100, 100]);
        set_variant_size_map(source.playlist_state.as_ref(), 1, &[100, 100]);

        // Layout variant = 0, commit and evict segment 0
        push_segment(&source.segments, 0, 0, 0, 100);
        push_segment(&source.segments, 0, 1, 100, 100);

        let evicted = ResourceKey::from_url(
            &Url::parse("https://example.com/seg-0-0.m4s").expect("valid segment URL"),
        );
        let removed = source.segments.lock_sync().remove_resource(&evicted);
        assert!(removed);

        // ABR wants variant 1, but layout is still variant 0
        source.coord.abr_variant_index.store(1, Ordering::Release);

        assert!(
            source.queue_segment_request_for_offset(0, 7),
            "hole at offset 0 must queue a recovery request"
        );
        assert_eq!(
            source.coord.take_segment_request(),
            Some(SegmentRequest {
                variant: 0,
                segment_index: 0,
                seek_epoch: 7,
            }),
            "recovery must target the layout variant that owns the missing segment"
        );
    }

    #[kithara::test]
    fn wait_range_reissues_request_after_pending_request_is_cleared() {
        let mut source = build_test_source(1);
        source.coord.stopped.store(false, Ordering::Release);
        let request = SegmentRequest {
            variant: 0,
            segment_index: 0,
            seek_epoch: 0,
        };
        source.coord.requeue_segment_request(request);

        let coord = Arc::clone(&source.coord);
        let join = thread::spawn(move || {
            thread::sleep(StdDuration::from_millis(10));
            coord.clear_pending_segment_request(request);
            coord.condvar.notify_all();
        });

        let result = source.wait_range(0..1, Duration::from_millis(120));
        join.join()
            .expect("clear-pending helper thread must complete");

        assert!(
            matches!(result, Err(StreamError::Source(HlsError::Timeout(_)))),
            "without a downloader the wait should still end by timeout"
        );

        let queued = source
            .coord
            .take_segment_request()
            .expect("wait_range must requeue the request once pending state clears");
        assert_eq!(queued, request);
    }

    #[kithara::test]
    fn wait_range_replaces_mismatched_pending_request_for_same_epoch() {
        let mut source = build_source_with_size_map(&[100, 100, 100]);
        source.coord.stopped.store(false, Ordering::Release);

        source.coord.requeue_segment_request(SegmentRequest {
            variant: 0,
            segment_index: 2,
            seek_epoch: 0,
        });

        let result = source.wait_range(0..1, Duration::from_millis(80));

        assert!(
            matches!(result, Err(StreamError::Source(HlsError::Timeout(_)))),
            "without a downloader the wait should still end by timeout"
        );

        let queued = source
            .coord
            .take_segment_request()
            .expect("wait_range must replace stale demand with the current segment request");
        assert_eq!(
            queued,
            SegmentRequest {
                variant: 0,
                segment_index: 0,
                seek_epoch: 0,
            }
        );
    }

    #[kithara::test]
    fn demand_range_queues_request_for_unloaded_offset() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0, 3,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                segment_sizes: vec![100, 100, 100],
                offsets: vec![0, 100, 200],
                total: 300,
            },
        );
        let parsed = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));

        source.demand_range(150..151);

        let req = source
            .coord
            .take_segment_request()
            .expect("demand_range must queue an on-demand segment request");
        assert_eq!(req.variant, 0);
        assert_eq!(req.segment_index, 1);
    }

    #[kithara::test]
    fn demand_range_does_not_queue_last_segment_at_exact_total_bytes() {
        let source = build_source_with_size_map(&[100, 100, 100]);

        source.demand_range(300..301);
        assert!(
            source.coord.take_segment_request().is_none(),
            "offset at exact total_bytes must not fallback to the last segment"
        );
    }

    #[kithara::test]
    fn format_change_segment_range_prefers_metadata_for_stale_init_segment_offset() {
        // Need 3 segments so StreamIndex variant_map covers segment 1
        let cancel = CancellationToken::new();
        let variant = make_variant_state_with_segments(0, 3);
        let playlist_state = Arc::new(PlaylistState::new(vec![variant]));
        let parsed = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));

        source.playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 25,
                segment_sizes: vec![100, 100, 100],
                offsets: vec![0, 100, 200],
                total: 300,
            },
        );

        source.segments.lock_sync().commit_segment(
            0,
            1,
            SegmentData {
                init_len: 25,
                media_len: 75,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-1.m4s")
                    .expect("valid segment URL"),
            },
        );

        // Segment 1 has init but segment 0 is not committed yet.
        // Must return segment 0's metadata range (0..100) so the decoder
        // starts at offset 0 and sees the full stream.
        assert_eq!(source.format_change_segment_range(), Some(0..100));
    }

    #[kithara::test]
    fn format_change_segment_range_uses_layout_floor_when_no_segments_are_loaded() {
        let source = build_source_with_size_map(&[100, 100, 100, 100]);

        // With per-variant byte maps, layout_floor is always segment 0.
        // No segments committed → fallback to metadata for segment 0.
        assert_eq!(
            source.format_change_segment_range(),
            Some(0..100),
            "without committed segments, decoder-start fallback uses metadata for layout variant segment 0"
        );
    }

    #[kithara::test]
    fn layout_variant_preserves_total_length() {
        let source = build_source_with_size_map(&[100, 100, 100, 100]);

        assert_eq!(
            source.len(),
            Some(400),
            "total length must reflect all segments in the layout variant"
        );
    }

    #[kithara::test]
    fn set_seek_epoch_drains_pending_segment_requests() {
        let mut source = build_test_source(1);
        source.coord.requeue_segment_request(SegmentRequest {
            variant: 0,
            segment_index: 1,
            seek_epoch: 3,
        });
        source.coord.requeue_segment_request(SegmentRequest {
            variant: 0,
            segment_index: 2,
            seek_epoch: 4,
        });

        source.set_seek_epoch(9);

        assert!(
            source.coord.take_segment_request().is_none(),
            "set_seek_epoch must drain pending segment requests"
        );
    }

    #[kithara::test]
    fn set_seek_epoch_keeps_exact_eof_visible_until_seek_lands() {
        let mut source = build_source_with_size_map(&[100, 100, 100]);
        source.coord.stopped.store(false, Ordering::Release);
        source.coord.timeline().set_byte_position(300);
        source.coord.timeline().set_eof(true);

        source.set_seek_epoch(7);

        let result = source.wait_range(300..301, Duration::from_millis(50));

        assert!(
            matches!(result, Ok(WaitOutcome::Eof)),
            "exact EOF must stay observable during the seek-reset window"
        );
    }

    #[kithara::test]
    fn wait_range_uses_known_total_bytes_for_exact_eof() {
        let mut source = build_source_with_size_map(&[100, 100, 100]);
        source.coord.stopped.store(false, Ordering::Release);
        source.coord.timeline().set_byte_position(300);
        source.coord.timeline().set_eof(false);

        let result = source.wait_range(300..301, Duration::from_millis(50));

        assert!(
            matches!(result, Ok(WaitOutcome::Eof)),
            "exact EOF must not depend on a separately refreshed eof flag when total_bytes is known"
        );
    }

    #[kithara::test]
    fn read_media_segment_checked_reads_active_resource_in_ephemeral_mode() {
        let source = build_test_source(1);
        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = source.fetch.backend().acquire_resource(&media_key).unwrap();
        media_res.write_at(0, b"media_data").unwrap();

        let seg = ReadSegment {
            variant: 0,
            segment_index: 0,
            byte_offset: 0,
            init_len: 0,
            media_len: 10,
            init_url: None,
            media_url,
        };
        let mut buf = [0u8; 10];

        let read = source
            .read_media_segment_checked(&seg, 0, &mut buf)
            .unwrap();

        assert_eq!(read, Some(10));
        assert_eq!(&buf, b"media_data");
    }

    #[kithara::test]
    fn read_at_does_not_advance_timeline_position() {
        let mut source = build_test_source(1);
        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = source.fetch.backend().acquire_resource(&media_key).unwrap();
        media_res.write_at(0, b"media_data").unwrap();
        media_res.commit(Some(10)).unwrap();

        source.segments.lock_sync().commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 10,
                init_url: None,
                media_url,
            },
        );
        source.coord.timeline().set_byte_position(0);

        let mut buf = [0u8; 10];
        let read = source.read_at(0, &mut buf).unwrap();

        assert_eq!(read, ReadOutcome::Data(10));
        assert_eq!(
            source.coord.timeline().byte_position(),
            0,
            "HLS read_at must not commit the reader cursor outside Stream::read"
        );
    }

    #[kithara::test]
    fn read_at_missing_segment_before_effective_total_returns_retry() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0, 3,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                segment_sizes: vec![100, 100, 100],
                offsets: vec![0, 100, 200],
                total: 300,
            },
        );
        let parsed = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, mut source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));

        source.segments.lock_sync().commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-0.m4s")
                    .expect("valid segment URL"),
            },
        );

        let mut buf = [0u8; 32];
        let read = source.read_at(150, &mut buf).unwrap();

        assert_eq!(
            read,
            ReadOutcome::Retry,
            "layout hole before effective total must trigger retry instead of synthetic EOF"
        );
    }

    #[kithara::test]
    fn wait_range_allows_short_read_when_range_crosses_known_eof() {
        let mut source = build_source_with_size_map(&[300]);
        source.coord.stopped.store(false, Ordering::Release);

        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = source.fetch.backend().acquire_resource(&media_key).unwrap();
        media_res.write_at(0, &[0u8; 300]).unwrap();
        media_res.commit(Some(300)).unwrap();

        source.segments.lock_sync().commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 300,
                init_url: None,
                media_url,
            },
        );

        let result = source.wait_range(250..350, Duration::from_millis(50));

        assert!(
            matches!(result, Ok(WaitOutcome::Ready)),
            "range that starts before EOF must be readable even if it extends past EOF"
        );
    }

    #[kithara::test]
    fn read_at_disk_reopened_segments_return_committed_bytes_after_eviction() {
        let cancel = CancellationToken::new();
        let dir = tempdir().expect("temp dir");
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0, 4,
        )]));
        let parsed = parsed_variants(1);
        let fetch = test_disk_fetch_manager(cancel.clone(), dir.path());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (_downloader, mut source) =
            build_pair(fetch, &parsed, &config, playlist_state, EventBus::new(16));

        let mut segments = Vec::new();
        for index in 0..4 {
            let media_url =
                Url::parse(&format!("https://example.com/seg-0-{index}.m4s")).expect("valid URL");
            let media_key = ResourceKey::from_url(&media_url);
            let payload: Vec<u8> = (0u8..=u8::MAX)
                .cycle()
                .skip(index * 13)
                .take(128 * 1024 + index * 1024 + 17)
                .collect();
            let res = source
                .fetch
                .backend()
                .acquire_resource(&media_key)
                .expect("open media resource");
            res.write_at(0, &payload).expect("write media");
            res.commit(Some(payload.len() as u64))
                .expect("commit media");
            source.segments.lock_sync().commit_segment(
                0,
                index,
                SegmentData {
                    init_len: 0,
                    media_len: payload.len() as u64,
                    init_url: None,
                    media_url,
                },
            );
            segments.push(payload);
        }

        let mut offset = 0u64;
        for payload in &segments {
            let mut buf = vec![0u8; payload.len()];
            let read = source.read_at(offset, &mut buf).expect("read_at");
            assert_eq!(read, ReadOutcome::Data(payload.len()));
            assert_eq!(buf, *payload);
            offset += payload.len() as u64;
        }
    }

    // Source::phase() trait method tests

    /// Build source for phase tests — resets `stopped` flag that
    /// `HlsDownloader::drop` sets when the downloader half is discarded.
    fn build_phase_test_source(num_variants: usize) -> HlsSource {
        let source = build_test_source(num_variants);
        source.coord.stopped.store(false, Ordering::Release);
        source
    }

    #[kithara::test]
    fn hls_phase_ready_when_range_ready() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        // Write actual resource data so ephemeral backend reports it as present.
        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 100]).unwrap();
        res.commit(Some(100)).unwrap();

        assert_eq!(source.phase_at(0..50), SourcePhase::Ready);
    }

    #[kithara::test]
    fn hls_phase_waiting_when_active_segment_does_not_cover_requested_range() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 16]).unwrap();

        assert_eq!(source.phase_at(0..16), SourcePhase::Ready);
        assert_eq!(source.phase_at(0..50), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn hls_phase_seeking_when_flushing() {
        let source = build_phase_test_source(1);
        let _ = source
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(0));

        assert_eq!(source.phase_at(0..50), SourcePhase::Seeking);
    }

    #[kithara::test]
    fn hls_phase_eof_when_past_effective_total() {
        let source = build_source_with_size_map(&[100]);
        source.coord.stopped.store(false, Ordering::Release);
        push_segment(&source.segments, 0, 0, 0, 100);
        source.coord.timeline().set_eof(true);

        assert_eq!(source.phase_at(200..250), SourcePhase::Eof);
    }

    #[kithara::test]
    fn hls_phase_cancelled_when_cancel_token_set() {
        let source = build_test_source(1);
        source.coord.cancel.cancel();

        assert_eq!(source.phase_at(0..50), SourcePhase::Cancelled);
    }

    #[kithara::test]
    fn hls_phase_cancelled_when_stopped() {
        let source = build_test_source(1);
        source.coord.stopped.store(true, Ordering::Release);

        assert_eq!(source.phase_at(0..50), SourcePhase::Cancelled);
    }

    #[kithara::test]
    fn hls_phase_waiting_when_no_data() {
        let source = build_phase_test_source(1);

        assert_eq!(source.phase_at(0..50), SourcePhase::Waiting);
    }

    // Source::phase() parameterless override tests

    #[kithara::test]
    fn hls_phase_parameterless_ready_when_segment_loaded() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        // Write actual resource data so ephemeral backend reports it as present.
        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 100]).unwrap();
        res.commit(Some(100)).unwrap();

        assert_eq!(source.phase(), SourcePhase::Ready);
    }

    #[kithara::test]
    fn hls_phase_parameterless_waiting_when_segment_only_partially_streamed() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 16]).unwrap();

        assert_eq!(source.phase(), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn hls_phase_parameterless_waiting_when_no_segments() {
        let source = build_phase_test_source(1);

        assert_eq!(source.phase(), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn fallback_seek_event_fires_at_most_once_per_offset() {
        let source = build_test_source(1);
        source.coord.stopped.store(false, Ordering::Release);
        let mut rx = source.bus.subscribe();

        // Call request_on_demand_segment 50 times at offset=0.
        // Without dedup this would publish 50 Seek events.
        for _ in 0..50 {
            let _ = source.request_on_demand_segment(0, 0);
        }

        // Count Seek events with stage "wait_range_metadata_fallback".
        let mut fallback_count = 0usize;
        while let Ok(event) = rx.try_recv() {
            if let kithara_events::Event::Hls(HlsEvent::Seek { stage, .. }) = event {
                if stage == "wait_range_metadata_fallback" {
                    fallback_count += 1;
                }
            }
        }

        assert!(
            fallback_count <= 1,
            "fallback seek event must fire at most once per (variant, offset), got {fallback_count}"
        );
    }
}
