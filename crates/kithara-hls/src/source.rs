//! HLS source: random-access reading from loaded HLS segments.
//!
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! Shared state with `HlsDownloader` (in `downloader.rs`) via `SharedSegments`.

use std::{
    collections::HashSet,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};

use crossbeam_queue::SegQueue;
use kithara_abr::Variant;
use kithara_assets::ResourceKey;
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{
    Condvar, Mutex,
    time::{Duration, Instant},
    tokio,
    tokio::sync::Notify,
};
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource, WaitOutcome};
use kithara_stream::{
    MediaInfo, ReadOutcome, Source, SourceSeekAnchor, StreamError, StreamResult, Timeline,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    HlsError,
    download_state::{DownloadProgress, DownloadState, LoadedSegment},
    downloader::{HlsDownloader, HlsIo},
    fetch::DefaultFetchManager,
    playlist::{PlaylistAccess, PlaylistState},
};

#[path = "source_wait_range.rs"]
mod source_wait_range;
use source_wait_range::{WaitRangeDecision, WaitRangeState};

/// Request to load a specific segment (on-demand or sequential).
#[derive(Debug, Clone)]
pub struct SegmentRequest {
    pub segment_index: usize,
    pub variant: usize,
    pub seek_epoch: u64,
}

/// Shared state between `HlsDownloader` and `HlsSource`.
pub struct SharedSegments {
    pub segments: Mutex<DownloadState>,
    /// Downloader → Source: new segment available (for sync blocking in Source).
    pub condvar: Condvar,
    /// Shared stream timeline (single source of truth for byte position).
    pub timeline: Timeline,
    /// Source → Downloader: reader advanced, may resume downloading.
    pub reader_advanced: Notify,
    /// Segment load requests (on-demand from seek or sequential).
    pub segment_requests: SegQueue<SegmentRequest>,
    /// Parsed playlist data (variant info, segment URLs, size maps).
    pub playlist_state: Arc<PlaylistState>,
    /// True after a mid-stream variant switch. On-demand loading should
    /// just wake the sequential downloader instead of using metadata lookups.
    pub had_midstream_switch: AtomicBool,
    /// Cancellation token for interrupting `wait_range`.
    pub cancel: CancellationToken,
    /// Downloader has exited (normally or with error).
    pub stopped: AtomicBool,
    /// Current segment index (updated by Source on each `read_at`).
    pub current_segment_index: Arc<AtomicU32>,
    /// Current variant index shared with ABR controller.
    pub abr_variant_index: Arc<AtomicUsize>,
}

impl SharedSegments {
    #[must_use]
    pub fn new(
        cancel: CancellationToken,
        playlist_state: Arc<PlaylistState>,
        timeline: Timeline,
    ) -> Self {
        Self::with_variant_index(
            cancel,
            playlist_state,
            timeline,
            Arc::new(AtomicUsize::new(0)),
        )
    }

    #[must_use]
    pub fn with_variant_index(
        cancel: CancellationToken,
        playlist_state: Arc<PlaylistState>,
        timeline: Timeline,
        abr_variant_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            segments: Mutex::new(DownloadState::new()),
            condvar: Condvar::new(),
            timeline,
            reader_advanced: Notify::new(),
            segment_requests: SegQueue::new(),
            playlist_state,
            had_midstream_switch: AtomicBool::new(false),
            cancel,
            stopped: AtomicBool::new(false),
            current_segment_index: Arc::new(AtomicU32::new(0)),
            abr_variant_index,
        }
    }
}

/// HLS source: provides random-access reading from loaded segments.
///
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct HlsSource {
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) shared: Arc<SharedSegments>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    pub(crate) variant_fence: Option<usize>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    pub(crate) _backend: Option<kithara_stream::Backend>,
}

const WAIT_RANGE_MAX_METADATA_MISS_SPINS: usize = 20;
const WAIT_RANGE_SLEEP_MS: u64 = 50;

impl HlsSource {
    /// Single source of truth for current variant resolution.
    /// Prefers `variant_fence` (set by `read_at`), falls back to ABR hint.
    fn resolve_current_variant(&self) -> usize {
        self.variant_fence
            .unwrap_or_else(|| self.shared.abr_variant_index.load(Ordering::Acquire))
    }

    fn current_loaded_segment(&self) -> Option<LoadedSegment> {
        let reader_offset = self.shared.timeline.byte_position();
        let segments = self.shared.segments.lock_sync();
        segments
            .find_at_offset(reader_offset)
            .or_else(|| segments.last())
            .cloned()
    }

    pub(crate) fn can_cross_variant_without_reset(
        &self,
        from_variant: usize,
        to_variant: usize,
    ) -> bool {
        self.playlist_state.variant_codec(from_variant)
            == self.playlist_state.variant_codec(to_variant)
    }

    fn byte_offset_for_segment(&self, variant: usize, segment_index: usize) -> Option<u64> {
        self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .or_else(|| {
                let segments = self.shared.segments.lock_sync();
                segments
                    .find_loaded_segment(variant, segment_index)
                    .map(|seg| seg.byte_offset)
            })
    }

    fn resolve_seek_anchor(&self, position: Duration) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        let mut variant = self.shared.abr_variant_index.load(Ordering::Acquire);
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
            let resource = self.fetch.backend().open_resource(&key)?;

            // TOCTOU guard: after eviction open_resource creates a fresh
            // empty (Active) resource. Committed means data is present.
            if self.fetch.backend().is_ephemeral()
                && matches!(resource.status(), ResourceStatus::Active)
            {
                return Ok(None);
            }

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
        let resource = self.fetch.backend().open_resource(&key)?;

        if self.fetch.backend().is_ephemeral()
            && matches!(resource.status(), ResourceStatus::Active)
        {
            return Ok(None);
        }

        let read_end = (media_offset + buf.len() as u64).min(seg.media_len);
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(Some(bytes_read))
    }

    pub(crate) fn range_ready_from_segments(
        &self,
        segments: &DownloadState,
        range: &Range<u64>,
    ) -> bool {
        let Some(seg) = segments.find_at_offset(range.start) else {
            return false;
        };

        // Disk-backed stores can reopen committed files after LRU eviction.
        // Ephemeral stores lose data on eviction — metadata in DownloadState
        // survives but bytes are gone. Verify LRU presence before claiming ready.
        if !self.fetch.backend().is_ephemeral() {
            return true;
        }

        if seg.init_len > 0
            && let Some(ref init_url) = seg.init_url
            && !self
                .fetch
                .backend()
                .has_resource(&ResourceKey::from_url(init_url))
        {
            return false;
        }
        self.fetch
            .backend()
            .has_resource(&ResourceKey::from_url(&seg.media_url))
    }

    fn push_segment_request(&self, variant: usize, segment_index: usize, seek_epoch: u64) {
        self.shared.segment_requests.push(SegmentRequest {
            segment_index,
            variant,
            seek_epoch,
        });
        self.shared.reader_advanced.notify_one();
    }
}

impl Source for HlsSource {
    type Error = HlsError;

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock must be held for condvar wait"
    )]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Duration,
    ) -> StreamResult<WaitOutcome, HlsError> {
        let mut state = WaitRangeState::default();

        kithara_platform::hang_watchdog! {
            thread: "hls.wait_range";
            loop {
                let mut segments = self.shared.segments.lock_sync();
                let context = self.build_wait_range_context(&segments, &range);
                state.reset_for_seek_epoch(context.seek_epoch);

                // Reset hang detector only when data covering our range is
                // available. Do NOT reset on total growth alone — a downloader
                // re-downloading unrelated segments (e.g. segment 0 in a loop)
                // must not mask a hang where the *needed* segment is missing.
                if context.range_ready {
                    hang_reset!();
                }

                match self.decide_wait_range(&range, &context) {
                    WaitRangeDecision::Cancelled => {
                        return Err(StreamError::Source(HlsError::Cancelled));
                    }
                    WaitRangeDecision::Continue => {
                        debug!(
                            range_start = range.start,
                            range_end = range.end,
                            eof = context.eof,
                            total = context.total,
                            expected_total = context.expected_total,
                            num_entries = context.num_entries,
                            range_ready = context.range_ready,
                            "wait_range: spinning (condition not met)"
                        );
                    }
                    WaitRangeDecision::Eof => return Ok(WaitOutcome::Eof),
                    WaitRangeDecision::Interrupted => return Ok(WaitOutcome::Interrupted),
                    WaitRangeDecision::Ready => return Ok(WaitOutcome::Ready),
                }

                if context.eof && !context.range_ready {
                    state.clear_on_demand_requested();
                }

                // A midstream variant switch drains all segment_requests.
                // Clear `on_demand_pending` so we can re-push for the new variant.
                // Use swap to consume the flag — prevents duplicate re-pushes.
                if self.shared.had_midstream_switch.swap(false, Ordering::AcqRel) {
                    state.clear_on_demand_requested();
                }

                drop(segments);
                self.request_on_demand_if_needed(range.start, context.seek_epoch, &mut state)?;
                segments = self.shared.segments.lock_sync();

                hang_tick!();
                kithara_platform::thread::yield_now();
                let deadline = Instant::now() + Duration::from_millis(WAIT_RANGE_SLEEP_MS);
                let (_segments, _wait_result) =
                    self.shared.condvar.wait_sync_timeout(segments, deadline);

                if self.shared.timeline.is_flushing() {
                    return Ok(WaitOutcome::Interrupted);
                }
            }
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, HlsError> {
        let seg = {
            let segments = self.shared.segments.lock_sync();
            segments.find_at_offset(offset).cloned()
        };

        let Some(seg) = seg else {
            return Ok(ReadOutcome::Data(0));
        };

        let previous_hint = self.shared.current_segment_index.load(Ordering::Acquire) as usize;
        #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
        self.shared
            .current_segment_index
            .store(seg.segment_index as u32, Ordering::Relaxed);
        if seg.segment_index < previous_hint {
            self.bus.publish(HlsEvent::Seek {
                stage: "read_at_moved_hint_backward",
                seek_epoch: self.shared.timeline.seek_epoch(),
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
            let seek_epoch = self.shared.timeline.seek_epoch();
            self.push_segment_request(seg.variant, seg.segment_index, seek_epoch);
            return Ok(ReadOutcome::Retry);
        };

        if bytes > 0 {
            let new_pos = offset.saturating_add(bytes as u64);
            self.shared.timeline.set_byte_position(new_pos);
            self.shared.reader_advanced.notify_one();

            let total = self.shared.segments.lock_sync().max_end_offset();
            self.bus.publish(HlsEvent::ByteProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(ReadOutcome::Data(bytes))
    }

    fn len(&self) -> Option<u64> {
        self.shared.timeline.total_bytes()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.shared.abr_variant_index.load(Ordering::Acquire);
        let reader_variant = self.current_loaded_segment().map(|seg| seg.variant);
        let has_hinted_variant = self
            .shared
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
        let current_variant = self.shared.abr_variant_index.load(Ordering::Acquire);
        {
            let segments = self.shared.segments.lock_sync();
            // Decoder recreate needs init-bearing segment (ftyp/moov),
            // not just the first loaded segment of the variant.
            if let Some(seg) = segments.first_init_segment_of_variant(current_variant) {
                return Some(seg.byte_offset..seg.end_offset());
            }

            let reader_offset = self.shared.timeline.byte_position();
            let fallback_variant = segments
                .find_at_offset(reader_offset)
                .or_else(|| segments.last())
                .map(|seg| seg.variant);
            if let Some(fallback_variant) = fallback_variant
                && let Some(seg) = segments.first_init_segment_of_variant(fallback_variant)
            {
                return Some(seg.byte_offset..seg.end_offset());
            }
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
        self.shared.condvar.notify_all();
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let shared = Arc::clone(&self.shared);
        Some(Box::new(move || {
            shared.condvar.notify_all();
        }))
    }

    fn set_seek_epoch(&mut self, _seek_epoch: u64) {
        // seek_epoch is now managed by Timeline.initiate_seek()
        self.shared.timeline.set_eof(false);
        self.shared.timeline.set_download_position(0);
        self.shared
            .had_midstream_switch
            .store(false, Ordering::Release);
        while self.shared.segment_requests.pop().is_some() {}
        {
            let mut segments = self.shared.segments.lock_sync();
            segments.clear();
        }
        self.shared
            .current_segment_index
            .store(0, Ordering::Release);
        self.shared.reader_advanced.notify_one();
        self.shared.condvar.notify_all();
    }

    fn seek_time_anchor(
        &mut self,
        position: Duration,
    ) -> StreamResult<Option<SourceSeekAnchor>, HlsError> {
        let anchor = self
            .resolve_seek_anchor(position)
            .map_err(StreamError::Source)?;
        let variant = anchor.variant_index.unwrap_or(0);
        let segment_index = anchor.segment_index.unwrap_or(0) as usize;
        let seek_epoch = self.shared.timeline.seek_epoch();
        let previous_hint = self.shared.current_segment_index.load(Ordering::Acquire) as usize;

        while self.shared.segment_requests.pop().is_some() {}
        self.shared.segment_requests.push(SegmentRequest {
            segment_index,
            variant,
            seek_epoch,
        });

        self.shared.timeline.set_byte_position(anchor.byte_offset);
        self.shared.reader_advanced.notify_one();
        self.shared.condvar.notify_all();
        self.shared
            .current_segment_index
            .store(anchor.segment_index.unwrap_or(0), Ordering::Relaxed);
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

        debug!(
            seek_epoch,
            target_ms = position.as_millis(),
            variant,
            segment_index,
            byte_offset = anchor.byte_offset,
            "seek_time_anchor: resolved and queued on-demand segment"
        );

        Ok(Some(anchor))
    }

    fn timeline(&self) -> Timeline {
        self.shared.timeline.clone()
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
    let shared = Arc::new(SharedSegments::with_variant_index(
        cancel,
        Arc::clone(&playlist_state),
        timeline,
        abr_variant_index,
    ));
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
        current_segment_index: 0,
        gap_scan_start_segment: 0,
        last_committed_variant: None,
        force_init_for_seek: false,
        sent_init_for_variant: HashSet::new(),
        abr,
        shared: Arc::clone(&shared),
        bus: bus.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        look_ahead_segments,
        prefetch_count: config.download_batch_size.max(1),
    };

    let source = HlsSource {
        fetch,
        shared,
        playlist_state,
        bus,
        variant_fence: None,
        _backend: None,
    };

    (downloader, source)
}

impl HlsSource {
    /// Set the backend (called after downloader is spawned).
    pub(crate) fn set_backend(&mut self, backend: kithara_stream::Backend) {
        self._backend = Some(backend);
    }

    /// Handle to current segment index atomic.
    pub(crate) fn segment_index_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.shared.current_segment_index)
    }

    /// Handle to current variant index atomic.
    pub(crate) fn variant_index_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.shared.abr_variant_index)
    }
}
