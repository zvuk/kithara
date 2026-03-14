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

use kithara_abr::Variant;
use kithara_assets::{AssetResourceState, ResourceKey};
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{
    Condvar, Mutex,
    time::{Duration, Instant},
    tokio,
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, StorageResource, WaitOutcome};
use kithara_stream::{
    DownloadCursor, MediaInfo, ReadOutcome, Source, SourceSeekAnchor, StreamError, StreamResult,
    Timeline,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};

use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    download_state::{DownloadProgress, DownloadState, LoadedSegment},
    downloader::{HlsDownloader, HlsIo},
    fetch::DefaultFetchManager,
    layout::{expected_layout_offset, find_segment_at_layout_offset},
    playlist::{PlaylistAccess, PlaylistState},
};

/// HLS source: provides random-access reading from loaded segments.
///
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct HlsSource {
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) segments: Arc<Mutex<DownloadState>>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<usize>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    pub(crate) _backend: Option<kithara_stream::Backend>,
}

const WAIT_RANGE_MAX_METADATA_MISS_SPINS: usize = 10;
const WAIT_RANGE_SLEEP_MS: u64 = 50;

/// Seek classification: whether the committed byte layout is preserved or reset.
enum SeekLayout {
    /// Same variant — keep `DownloadState`, byte layout unchanged.
    Preserve,
    /// Different variant — clear `DownloadState`, rebuild layout.
    Reset,
}

#[derive(Clone, Copy)]
enum ResolvedSegment {
    Committed(usize),
    Layout(usize),
    Fallback(usize),
}

impl HlsSource {
    // --- construction / handles ---

    /// Set the backend (called after downloader is spawned).
    pub(crate) fn set_backend(&mut self, backend: kithara_stream::Backend) {
        self._backend = Some(backend);
    }

    // --- variant resolution ---

    /// Single source of truth for current variant resolution.
    /// Prefers `variant_fence` (set by `read_at`), falls back to ABR hint.
    fn resolve_current_variant(&self) -> usize {
        self.variant_fence
            .unwrap_or_else(|| self.coord.abr_variant_index.load(Ordering::Acquire))
    }

    pub(crate) fn can_cross_variant_without_reset(
        &self,
        from_variant: usize,
        to_variant: usize,
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
    fn current_layout_variant(&self) -> Option<usize> {
        let pos = self.coord.timeline().byte_position();
        let segments = self.segments.lock_sync();
        if let Some(seg) = segments.find_at_offset(pos) {
            return Some(seg.variant);
        }
        if let Some(seg) = segments.last() {
            return Some(seg.variant);
        }
        drop(segments);
        self.variant_fence
    }

    // --- segment lookup ---

    fn current_loaded_segment(&self) -> Option<LoadedSegment> {
        let reader_offset = self.coord.timeline().byte_position();
        let segments = self.segments.lock_sync();
        segments
            .find_at_offset(reader_offset)
            .or_else(|| segments.last())
            .cloned()
    }

    fn current_segment_index(&self) -> Option<usize> {
        self.current_loaded_segment().map(|seg| seg.segment_index)
    }

    fn byte_offset_for_segment(&self, variant: usize, segment_index: usize) -> Option<u64> {
        let allow_infer = self.current_layout_variant() == Some(variant);
        let had_midstream_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
        {
            let segments = self.segments.lock_sync();
            if let Some(seg) = segments.find_loaded_segment(variant, segment_index) {
                return Some(seg.byte_offset);
            }
            let offset = expected_layout_offset(
                &self.playlist_state,
                &segments,
                variant,
                segment_index,
                allow_infer,
                had_midstream_switch,
            );
            drop(segments);
            offset
        }
    }

    fn metadata_range_for_segment(
        &self,
        variant: usize,
        segment_index: usize,
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

    fn init_segment_range_for_variant(&self, variant: usize) -> Option<Range<u64>> {
        let init_segment = {
            let segments = self.segments.lock_sync();
            segments.first_init_segment_of_variant(variant).cloned()
        }?;

        if init_segment.init_url.is_none()
            && let Some(metadata_range) =
                self.metadata_range_for_segment(variant, init_segment.segment_index)
        {
            return Some(metadata_range);
        }

        Some(init_segment.byte_offset..init_segment.end_offset())
    }

    fn fallback_segment_index_for_offset(&self, variant: usize, offset: u64) -> Option<usize> {
        let num_segments = self.playlist_state.num_segments(variant)?;
        if num_segments == 0 {
            return None;
        }

        if let Some(total) = self.coord.timeline().total_bytes()
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

    /// Check committed `DownloadState` for the segment covering `range_start`.
    /// Returns `Some(segment_index)` if a request should be issued.
    fn committed_segment_for_offset(&self, range_start: u64, variant: usize) -> Option<usize> {
        {
            let segments = self.segments.lock_sync();
            if let Some(seg) = segments.find_at_offset(range_start)
                && seg.variant == variant
            {
                return Some(seg.segment_index);
            }
        }

        None
    }

    // --- phase / state ---

    /// Check if the read position is past the effective end of stream.
    fn is_past_eof(&self, segments: &DownloadState, range: &Range<u64>) -> bool {
        let known_total = self.coord.timeline().total_bytes();
        let loaded_total = segments.max_end_offset();
        let effective_total = loaded_total.max(known_total.unwrap_or(0));
        if effective_total == 0 || range.start < effective_total {
            return false;
        }

        known_total.is_some() || self.coord.timeline().eof()
    }

    pub(crate) fn range_ready_from_segments(
        &self,
        segments: &DownloadState,
        range: &Range<u64>,
    ) -> bool {
        let Some(seg) = segments.find_at_offset(range.start) else {
            return false;
        };

        let range_end = range.end.min(seg.end_offset());
        let local_start = range.start.saturating_sub(seg.byte_offset);
        let local_end = range_end.saturating_sub(seg.byte_offset);

        let init_end = seg.init_len.min(local_end);
        if local_start < init_end {
            let Some(ref init_url) = seg.init_url else {
                return false;
            };
            if !self.resource_covers_range(&ResourceKey::from_url(init_url), local_start..init_end)
            {
                return false;
            }
        }

        let media_start = local_start.max(seg.init_len).saturating_sub(seg.init_len);
        let media_end = local_end.saturating_sub(seg.init_len);
        if media_start < media_end
            && !self.resource_covers_range(
                &ResourceKey::from_url(&seg.media_url),
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

    // --- read ---

    /// Read from a loaded segment.
    ///
    /// Returns `Ok(None)` when the resource was evicted from the LRU cache
    /// between `wait_range` (metadata ready) and this read attempt.
    /// The caller should convert this to `ReadOutcome::Retry`.
    fn read_from_entry(
        &self,
        seg: &LoadedSegment,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let local_offset = offset - seg.byte_offset;

        if local_offset < seg.init_len {
            let Some(ref init_url) = seg.init_url else {
                return Ok(Some(0));
            };

            let key = ResourceKey::from_url(init_url);
            if self.fetch.backend().is_ephemeral() {
                match self.fetch.backend().resource_state(&key)? {
                    AssetResourceState::Active | AssetResourceState::Committed { .. } => {}
                    _ => return Ok(None),
                }
            }
            let resource = self.fetch.backend().open_resource(&key)?;

            let read_end = (local_offset + buf.len() as u64).min(seg.init_len);
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
        seg: &LoadedSegment,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<Option<usize>, HlsError> {
        let key = ResourceKey::from_url(&seg.media_url);
        if self.fetch.backend().is_ephemeral() {
            match self.fetch.backend().resource_state(&key)? {
                AssetResourceState::Active | AssetResourceState::Committed { .. } => {}
                _ => return Ok(None),
            }
        }
        let resource = self.fetch.backend().open_resource(&key)?;

        let read_end = (media_offset + buf.len() as u64).min(seg.media_len);
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(Some(bytes_read))
    }

    // --- seek ---

    fn resolve_seek_anchor(&self, position: Duration) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        let mut variant = self.coord.abr_variant_index.load(Ordering::Acquire);
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

    /// Classify a seek as Preserve (keep segments) or Reset (clear segments).
    fn classify_seek(&self, anchor: &SourceSeekAnchor) -> SeekLayout {
        let target_variant = anchor.variant_index.unwrap_or(0);
        match self.current_layout_variant() {
            Some(current) if current == target_variant => SeekLayout::Preserve,
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
                // Keep segments — byte layout valid, Symphonia tables remain correct.
                trace!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Preserve — keeping DownloadState"
                );
            }
            SeekLayout::Reset => {
                // Clear segments — layout changes, decoder will be recreated.
                let mut segments = self.segments.lock_sync();
                segments.clear();
                drop(segments);
                self.coord.timeline().set_download_position(0);
                debug!(
                    seek_epoch,
                    variant,
                    segment_index,
                    byte_offset = anchor.byte_offset,
                    "seek plan: Reset — cleared DownloadState"
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

    // --- on-demand segment requests ---

    fn push_segment_request(&self, variant: usize, segment_index: usize, seek_epoch: u64) -> bool {
        self.coord.enqueue_segment_request(SegmentRequest {
            segment_index,
            variant,
            seek_epoch,
        })
    }

    fn queue_segment_request_for_offset(&self, range_start: u64, seek_epoch: u64) -> bool {
        let current_variant = self.resolve_current_variant();

        if let Some(segment_index) = self
            .resolve_segment_for_offset(range_start, current_variant)
            .map(Self::segment_index)
        {
            return self.push_segment_request(current_variant, segment_index, seek_epoch);
        }

        false
    }

    fn resolve_segment_for_offset(
        &self,
        range_start: u64,
        variant: usize,
    ) -> Option<ResolvedSegment> {
        if let Some(segment_index) = self.committed_segment_for_offset(range_start, variant) {
            return Some(ResolvedSegment::Committed(segment_index));
        }

        let allow_infer = self.current_layout_variant() == Some(variant);
        let had_midstream_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
        let segments = self.segments.lock_sync();
        if let Some(segment_index) = find_segment_at_layout_offset(
            &self.playlist_state,
            &segments,
            variant,
            range_start,
            allow_infer,
            had_midstream_switch,
        ) {
            return Some(ResolvedSegment::Layout(segment_index));
        }
        drop(segments);

        self.fallback_segment_index_for_offset(variant, range_start)
            .map(ResolvedSegment::Fallback)
    }

    fn segment_index(resolved: ResolvedSegment) -> usize {
        match resolved {
            ResolvedSegment::Committed(segment_index)
            | ResolvedSegment::Layout(segment_index)
            | ResolvedSegment::Fallback(segment_index) => segment_index,
        }
    }

    fn loaded_segment_ready(&self, variant: usize, segment_index: usize) -> bool {
        {
            let segments = self.segments.lock_sync();
            let Some(seg) = segments.find_loaded_segment(variant, segment_index) else {
                return false;
            };
            let range = seg.byte_offset..seg.end_offset();
            let ready = self.range_ready_from_segments(&segments, &range);
            drop(segments);
            ready
        }
    }

    #[expect(
        clippy::result_large_err,
        reason = "Stream source APIs use StreamResult<_, HlsError> consistently"
    )]
    fn request_on_demand_segment(
        &self,
        range_start: u64,
        seek_epoch: u64,
        metadata_miss_count: &mut usize,
        max_metadata_miss_spins: usize,
    ) -> StreamResult<bool, HlsError> {
        let current_variant = self.resolve_current_variant();
        if current_variant >= self.playlist_state.num_variants() {
            *metadata_miss_count = metadata_miss_count.saturating_add(1);
            self.bus.publish(HlsEvent::SeekMetadataMiss {
                seek_epoch,
                offset: range_start,
                variant: current_variant,
            });
            let error = format!(
                "seek metadata miss: offset={} variant={} epoch={} reason=variant_out_of_range",
                range_start, current_variant, seek_epoch
            );
            if *metadata_miss_count >= max_metadata_miss_spins {
                self.bus.publish(HlsEvent::Error {
                    error: error.clone(),
                    recoverable: false,
                });
                return Err(StreamError::Source(HlsError::SegmentNotFound(error)));
            }
            self.coord.reader_advanced.notify_one();
            return Ok(false);
        }

        if let Some(resolved) = self.resolve_segment_for_offset(range_start, current_variant) {
            *metadata_miss_count = 0;
            let segment_index = Self::segment_index(resolved);
            match resolved {
                ResolvedSegment::Committed(_) => trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: committed on-demand segment load"
                ),
                ResolvedSegment::Layout(_) => trace!(
                    variant = current_variant,
                    segment_index,
                    offset = range_start,
                    "wait_range: layout on-demand segment load"
                ),
                ResolvedSegment::Fallback(_) => {
                    let hint_index = self.current_segment_index().unwrap_or(0);
                    trace!(
                        variant = current_variant,
                        segment_index,
                        offset = range_start,
                        "wait_range: metadata miss fallback segment load"
                    );
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
            return Ok(self.push_segment_request(current_variant, segment_index, seek_epoch));
        }

        *metadata_miss_count = metadata_miss_count.saturating_add(1);
        debug!(
            offset = range_start,
            variant = current_variant,
            seek_epoch,
            metadata_miss_count = *metadata_miss_count,
            "wait_range: no metadata to find segment"
        );
        self.bus.publish(HlsEvent::SeekMetadataMiss {
            seek_epoch,
            offset: range_start,
            variant: current_variant,
        });
        self.coord.reader_advanced.notify_one();

        if *metadata_miss_count >= max_metadata_miss_spins {
            let error = format!(
                "seek metadata miss: offset={} variant={} epoch={} misses={}",
                range_start, current_variant, seek_epoch, *metadata_miss_count
            );
            self.bus.publish(HlsEvent::Error {
                error: error.clone(),
                recoverable: false,
            });
            return Err(StreamError::Source(HlsError::SegmentNotFound(error)));
        }

        Ok(false)
    }
}

impl Source for HlsSource {
    type Error = HlsError;
    type Topology = Arc<PlaylistState>;
    type Layout = Arc<Mutex<DownloadState>>;
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
    #[kithara_hang_detector::hang_watchdog(timeout = timeout)]
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
                return Ok(WaitOutcome::Ready);
            }
            if seeking {
                return Ok(WaitOutcome::Interrupted);
            }
            if past_eof {
                debug!(
                    range_start = range.start,
                    total_bytes = segments.max_end_offset(),
                    num_entries = segments.num_entries(),
                    "wait_range: EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            // Waiting sub-states — log and continue spinning.
            let phase = if waiting_metadata {
                kithara_stream::SourcePhase::WaitingMetadata
            } else if waiting_demand {
                kithara_stream::SourcePhase::WaitingDemand
            } else {
                kithara_stream::SourcePhase::Waiting
            };
            trace!(
                range_start = range.start,
                range_end = range.end,
                ?phase,
                "wait_range: spinning"
            );

            // A midstream variant switch drains all segment_requests.
            // Clear on-demand tracking so we can re-push for the new variant.
            self.coord
                .had_midstream_switch
                .swap(false, Ordering::AcqRel);

            drop(segments);

            // Request on-demand segment if not already pending.
            if !self.coord.has_pending_segment_request(seek_epoch) {
                self.request_on_demand_segment(
                    range.start,
                    seek_epoch,
                    &mut metadata_miss_count,
                    WAIT_RANGE_MAX_METADATA_MISS_SPINS,
                )?;
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
            kithara_platform::thread::yield_now();
            let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
            let (_segments, _wait_result) =
                self.coord.condvar.wait_sync_timeout(segments, deadline);

            if self.coord.timeline().is_flushing() {
                return Ok(WaitOutcome::Interrupted);
            }
        }
    }

    fn phase_at(&self, range: Range<u64>) -> kithara_stream::SourcePhase {
        use kithara_stream::SourcePhase;
        let segments = self.segments.lock_sync();
        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }
        if self.range_ready_from_segments(&segments, &range) {
            return SourcePhase::Ready;
        }
        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if self.is_past_eof(&segments, &range) {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    fn phase(&self) -> kithara_stream::SourcePhase {
        use kithara_stream::SourcePhase;

        let pos = self.coord.timeline().byte_position();

        if self.coord.cancel.is_cancelled() || self.coord.stopped.load(Ordering::Acquire) {
            return SourcePhase::Cancelled;
        }

        let (segment_ready, past_eof) = {
            let segments = self.segments.lock_sync();
            let ready = segments.find_at_offset(pos).is_some_and(|seg| {
                let seg_range = seg.byte_offset..seg.end_offset();
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
        let seg = {
            let segments = self.segments.lock_sync();
            segments.find_at_offset(offset).cloned()
        };

        let Some(seg) = seg else {
            return Ok(ReadOutcome::Data(0));
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
        self.coord.timeline().total_bytes()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
        let reader_variant = self.current_loaded_segment().map(|seg| seg.variant);
        let has_hinted_variant = self
            .segments
            .lock_sync()
            .first_segment_of_variant(hinted_variant)
            .is_some();
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
        self.current_loaded_segment()
            .map(|seg| seg.byte_offset..seg.end_offset())
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let current_variant = self.coord.abr_variant_index.load(Ordering::Acquire);
        if let Some(range) = self.init_segment_range_for_variant(current_variant) {
            return Some(range);
        }

        let fallback_variant = {
            let segments = self.segments.lock_sync();
            let reader_offset = self.coord.timeline().byte_position();
            segments
                .find_at_offset(reader_offset)
                .or_else(|| segments.last())
                .map(|seg| seg.variant)
        };
        if let Some(fallback_variant) = fallback_variant
            && let Some(range) = self.init_segment_range_for_variant(fallback_variant)
        {
            return Some(range);
        }

        // After seek flush, no segments may be loaded yet.
        // Fall back to metadata offsets so decoder recreation can start
        // from the variant's init-bearing first segment.
        if current_variant >= self.playlist_state.num_variants() {
            return None;
        }
        let start = self
            .playlist_state
            .segment_byte_offset(current_variant, 0)?;
        let end = self
            .playlist_state
            .segment_byte_offset(current_variant, 1)
            .or_else(|| self.playlist_state.total_variant_size(current_variant))?;
        if end > start { Some(start..end) } else { None }
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
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
        // Non-destructive: does NOT clear DownloadState or download_position.
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
        let variant = anchor
            .and_then(|resolved| resolved.variant_index)
            .unwrap_or_else(|| self.resolve_current_variant());
        let segment_index = self
            .resolve_segment_for_offset(landed_offset, variant)
            .map(Self::segment_index);
        {
            let mut segments = self.segments.lock_sync();
            segments.fence_at(landed_offset, variant);
        }
        self.variant_fence = Some(variant);
        let Some(segment_index) = segment_index else {
            debug!(
                seek_epoch,
                variant, landed_offset, "commit_seek_landing: could not resolve landed segment"
            );
            self.coord.condvar.notify_all();
            self.coord.reader_advanced.notify_one();
            return;
        };
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

    let mut abr_opts = config.abr.clone();
    abr_opts.variants = abr_variants;

    let cancel = config.cancel.clone().unwrap_or_default();
    let abr = kithara_abr::AbrController::new(abr_opts);
    let abr_variant_index = abr.variant_index_handle();
    let timeline = Timeline::new();
    timeline.set_total_duration(playlist_state.track_duration());
    let coord = Arc::new(HlsCoord::new(cancel, timeline, abr_variant_index));
    let segments = Arc::new(Mutex::new(DownloadState::new()));
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
        last_committed_variant: None,
        force_init_for_seek: false,
        sent_init_for_variant: HashSet::new(),
        abr,
        coord: Arc::clone(&coord),
        segments: Arc::clone(&segments),
        bus: bus.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        look_ahead_segments,
        prefetch_count: config.download_batch_size.max(1),
    };

    let source = HlsSource {
        coord,
        fetch,
        segments,
        playlist_state,
        bus,
        variant_fence: None,
        _backend: None,
    };

    (downloader, source)
}

#[cfg(test)]
mod tests {
    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_test_utils::kithara;
    use tokio::time::{Duration as TokioDuration, timeout};
    use url::Url;

    use super::*;
    use crate::{
        config::HlsConfig,
        download_state::LoadedSegment,
        fetch::{DefaultFetchManager, FetchManager},
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState},
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

    fn build_test_source(num_variants: usize) -> HlsSource {
        let cancel = CancellationToken::new();
        let variants: Vec<VariantState> = (0..num_variants).map(make_variant_state).collect();
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

    fn push_segment(
        segments: &Arc<Mutex<DownloadState>>,
        variant: usize,
        index: usize,
        offset: u64,
        len: u64,
    ) {
        let seg = LoadedSegment {
            variant,
            segment_index: index,
            byte_offset: offset,
            init_len: 0,
            media_len: len,
            init_url: None,
            media_url: Url::parse(&format!("https://example.com/seg-{variant}-{index}.m4s"))
                .expect("valid segment URL"),
        };
        segments.lock_sync().push(seg);
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
    fn cross_variant_seek_clears_segments() {
        let source = build_test_source(2);
        push_segment(&source.segments, 0, 0, 0, 100);

        let anchor = make_anchor(1, 0, 0);
        let layout = source.classify_seek(&anchor);
        assert!(matches!(layout, SeekLayout::Reset));
    }

    #[kithara::test]
    fn same_codec_different_variant_seek_clears_segments() {
        let source = build_test_source(2);
        push_segment(&source.segments, 0, 0, 0, 100);

        // Different variant index even with same codec → Reset
        let anchor = make_anchor(1, 0, 0);
        let layout = source.classify_seek(&anchor);
        assert!(matches!(layout, SeekLayout::Reset));
    }

    #[kithara::test(tokio)]
    async fn on_demand_request_notifies_downloader_once() {
        let source = build_test_source(1);
        let wake = source.coord.reader_advanced.notified();
        let mut miss_count = 0;

        source
            .request_on_demand_segment(0, 7, &mut miss_count, 5)
            .expect("request should succeed");

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

        let mut miss_count = 0;
        let requested = source
            .request_on_demand_segment(0, 7, &mut miss_count, 5)
            .expect("request should succeed");

        assert!(
            !requested,
            "request_on_demand_segment must report false when dedupe suppresses enqueue"
        );
        assert_eq!(
            source.coord.take_segment_request(),
            Some(request),
            "dedupe must keep the original pending request instead of enqueuing a duplicate"
        );
    }

    #[kithara::test]
    fn wait_range_reissues_request_after_pending_request_is_cleared() {
        use std::{thread, time::Duration as StdDuration};

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
        source.coord.timeline().set_total_bytes(Some(300));

        source.demand_range(300..301);
        assert!(
            source.coord.take_segment_request().is_none(),
            "offset at exact total_bytes must not fallback to the last segment"
        );
    }

    #[kithara::test]
    fn format_change_segment_range_prefers_metadata_for_stale_init_segment_offset() {
        let source = build_test_source(1);
        source.playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 25,
                segment_sizes: vec![100, 100, 100],
                offsets: vec![0, 100, 200],
                total: 300,
            },
        );

        source.segments.lock_sync().push(LoadedSegment {
            variant: 0,
            segment_index: 1,
            byte_offset: 10,
            init_len: 25,
            media_len: 75,
            init_url: None,
            media_url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid segment URL"),
        });

        assert_eq!(source.format_change_segment_range(), Some(100..200));
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
        let mut source = build_test_source(1);
        source.coord.stopped.store(false, Ordering::Release);
        source.coord.timeline().set_total_bytes(Some(300));
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
        let mut source = build_test_source(1);
        source.coord.stopped.store(false, Ordering::Release);
        source.coord.timeline().set_total_bytes(Some(300));
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

        let seg = LoadedSegment {
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

        source.segments.lock_sync().push(LoadedSegment {
            variant: 0,
            segment_index: 0,
            byte_offset: 0,
            init_len: 0,
            media_len: 10,
            init_url: None,
            media_url,
        });
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
    fn wait_range_allows_short_read_when_range_crosses_known_eof() {
        let mut source = build_test_source(1);
        source.coord.stopped.store(false, Ordering::Release);
        source.coord.timeline().set_total_bytes(Some(300));

        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = source.fetch.backend().acquire_resource(&media_key).unwrap();
        media_res.write_at(0, &[0u8; 300]).unwrap();
        media_res.commit(Some(300)).unwrap();

        source.segments.lock_sync().push(LoadedSegment {
            variant: 0,
            segment_index: 0,
            byte_offset: 0,
            init_len: 0,
            media_len: 300,
            init_url: None,
            media_url,
        });

        let result = source.wait_range(250..350, Duration::from_millis(50));

        assert!(
            matches!(result, Ok(WaitOutcome::Ready)),
            "range that starts before EOF must be readable even if it extends past EOF"
        );
    }

    // --- Source::phase() trait method tests ---

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

        assert_eq!(source.phase_at(0..50), kithara_stream::SourcePhase::Ready);
    }

    #[kithara::test]
    fn hls_phase_waiting_when_active_segment_does_not_cover_requested_range() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 16]).unwrap();

        assert_eq!(source.phase_at(0..16), kithara_stream::SourcePhase::Ready);
        assert_eq!(source.phase_at(0..50), kithara_stream::SourcePhase::Waiting);
    }

    #[kithara::test]
    fn hls_phase_seeking_when_flushing() {
        let source = build_phase_test_source(1);
        let _ = source
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(0));

        assert_eq!(source.phase_at(0..50), kithara_stream::SourcePhase::Seeking);
    }

    #[kithara::test]
    fn hls_phase_eof_when_past_effective_total() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);
        source.coord.timeline().set_eof(true);
        source.coord.timeline().set_total_bytes(Some(100));

        assert_eq!(source.phase_at(200..250), kithara_stream::SourcePhase::Eof);
    }

    #[kithara::test]
    fn hls_phase_cancelled_when_cancel_token_set() {
        let source = build_test_source(1);
        source.coord.cancel.cancel();

        assert_eq!(
            source.phase_at(0..50),
            kithara_stream::SourcePhase::Cancelled
        );
    }

    #[kithara::test]
    fn hls_phase_cancelled_when_stopped() {
        let source = build_test_source(1);
        source.coord.stopped.store(true, Ordering::Release);

        assert_eq!(
            source.phase_at(0..50),
            kithara_stream::SourcePhase::Cancelled
        );
    }

    #[kithara::test]
    fn hls_phase_waiting_when_no_data() {
        let source = build_phase_test_source(1);

        assert_eq!(source.phase_at(0..50), kithara_stream::SourcePhase::Waiting);
    }

    // --- Source::phase() parameterless override tests ---

    #[kithara::test]
    fn hls_phase_parameterless_ready_when_segment_loaded() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        // Write actual resource data so ephemeral backend reports it as present.
        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 100]).unwrap();
        res.commit(Some(100)).unwrap();

        assert_eq!(source.phase(), kithara_stream::SourcePhase::Ready);
    }

    #[kithara::test]
    fn hls_phase_parameterless_waiting_when_segment_only_partially_streamed() {
        let source = build_phase_test_source(1);
        push_segment(&source.segments, 0, 0, 0, 100);

        let key = ResourceKey::from_url(&"https://example.com/seg-0-0.m4s".parse::<Url>().unwrap());
        let res = source.fetch.backend().acquire_resource(&key).unwrap();
        res.write_at(0, &[0u8; 16]).unwrap();

        assert_eq!(source.phase(), kithara_stream::SourcePhase::Waiting);
    }

    #[kithara::test]
    fn hls_phase_parameterless_waiting_when_no_segments() {
        let source = build_phase_test_source(1);

        assert_eq!(source.phase(), kithara_stream::SourcePhase::Waiting);
    }
}
