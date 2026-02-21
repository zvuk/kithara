//! HLS source: random-access reading from loaded HLS segments.
//!
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! Shared state with `HlsDownloader` (in `downloader.rs`) via `SharedSegments`.

use std::{
    collections::HashSet,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
};

use crossbeam_queue::SegQueue;
use kithara_abr::Variant;
use kithara_assets::ResourceKey;
use kithara_events::{EventBus, HlsEvent};
use kithara_platform::{Condvar, Mutex, time::Duration};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{MediaInfo, Source, SourceSeekAnchor, StreamError, StreamResult, Timeline};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    HlsError,
    download_state::{DownloadProgress, DownloadState, LoadedSegment},
    downloader::{HlsDownloader, HlsIo},
    fetch::DefaultFetchManager,
    playlist::{PlaylistAccess, PlaylistState},
};

/// Request to load a specific segment (on-demand or sequential).
#[derive(Debug, Clone)]
pub(crate) struct SegmentRequest {
    pub(crate) segment_index: usize,
    pub(crate) variant: usize,
    pub(crate) seek_epoch: u64,
}

/// Shared state between `HlsDownloader` and `HlsSource`.
pub(crate) struct SharedSegments {
    pub(crate) segments: Mutex<DownloadState>,
    /// Downloader → Source: new segment available (for sync blocking in Source).
    pub(crate) condvar: Condvar,
    /// Shared stream timeline (single source of truth for byte position).
    pub(crate) timeline: Timeline,
    /// Source → Downloader: reader advanced, may resume downloading.
    pub(crate) reader_advanced: Notify,
    /// Segment load requests (on-demand from seek or sequential).
    pub(crate) segment_requests: SegQueue<SegmentRequest>,
    /// Parsed playlist data (variant info, segment URLs, size maps).
    pub(crate) playlist_state: Arc<PlaylistState>,
    /// True after a mid-stream variant switch. On-demand loading should
    /// just wake the sequential downloader instead of using metadata lookups.
    pub(crate) had_midstream_switch: AtomicBool,
    /// Cancellation token for interrupting `wait_range`.
    pub(crate) cancel: CancellationToken,
    /// Downloader has exited (normally or with error).
    pub(crate) stopped: AtomicBool,
    /// Current segment index (updated by Source on each `read_at`).
    pub(crate) current_segment_index: Arc<AtomicU32>,
    /// Current variant index (updated by Source on each `read_at`).
    pub(crate) current_variant_index: Arc<AtomicUsize>,
    /// Current seek generation used by pending requests.
    pub(crate) seek_epoch: AtomicU64,
}

impl SharedSegments {
    pub(crate) fn new(
        cancel: CancellationToken,
        playlist_state: Arc<PlaylistState>,
        timeline: Timeline,
    ) -> Self {
        Self {
            segments: Mutex::new(DownloadState::new()),
            condvar: Condvar::new(),
            timeline,
            reader_advanced: Notify::new(),
            segment_requests: SegQueue::new(),
            playlist_state,
            had_midstream_switch: AtomicBool::new(false),
            seek_epoch: AtomicU64::new(0),
            cancel,
            stopped: AtomicBool::new(false),
            current_segment_index: Arc::new(AtomicU32::new(0)),
            current_variant_index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// HLS source: provides random-access reading from loaded segments.
///
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct HlsSource {
    fetch: Arc<DefaultFetchManager>,
    shared: Arc<SharedSegments>,
    playlist_state: Arc<PlaylistState>,
    bus: EventBus,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    variant_fence: Option<usize>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    _backend: Option<kithara_stream::Backend>,
}

impl HlsSource {
    fn current_loaded_segment(&self) -> Option<LoadedSegment> {
        let reader_offset = self.shared.timeline.byte_position();
        let segments = self.shared.segments.lock();
        segments
            .find_at_offset(reader_offset)
            .or_else(|| segments.last())
            .cloned()
    }

    fn can_cross_variant_without_reset(&self, from_variant: usize, to_variant: usize) -> bool {
        self.playlist_state.variant_codec(from_variant)
            == self.playlist_state.variant_codec(to_variant)
    }

    fn byte_offset_for_segment(&self, variant: usize, segment_index: usize) -> Option<u64> {
        self.playlist_state
            .segment_byte_offset(variant, segment_index)
            .or_else(|| {
                let segments = self.shared.segments.lock();
                segments
                    .find_loaded_segment(variant, segment_index)
                    .map(|seg| seg.byte_offset)
            })
    }

    fn fallback_segment_index_for_offset(&self, variant: usize, offset: u64) -> Option<usize> {
        let num_segments = self.playlist_state.num_segments(variant)?;
        if num_segments == 0 {
            return None;
        }

        if let Some(total) = self.shared.timeline.total_bytes()
            && total > 0
        {
            let estimate = ((u128::from(offset) * num_segments as u128) / u128::from(total))
                .min((num_segments - 1) as u128);
            return Some(estimate as usize);
        }

        let hinted = self.shared.current_segment_index.load(Ordering::Acquire) as usize;
        Some(hinted.min(num_segments - 1))
    }

    fn resolve_seek_anchor(&self, position: Duration) -> Result<SourceSeekAnchor, HlsError> {
        let variants = self.playlist_state.num_variants();
        if variants == 0 {
            return Err(HlsError::SegmentNotFound("empty playlist".to_string()));
        }

        let mut variant = self.shared.current_variant_index.load(Ordering::Acquire);
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
    fn read_from_entry(
        &self,
        seg: &LoadedSegment,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let local_offset = offset - seg.byte_offset;

        if local_offset < seg.init_len {
            let Some(ref init_url) = seg.init_url else {
                return Ok(0);
            };

            let key = ResourceKey::from_url(init_url);
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
                let bytes_from_media = self.read_media_segment(seg, 0, remaining)?;
                Ok(bytes_from_init + bytes_from_media)
            } else {
                Ok(bytes_from_init)
            }
        } else {
            let media_offset = local_offset - seg.init_len;
            self.read_media_segment(seg, media_offset, buf)
        }
    }

    fn read_media_segment(
        &self,
        seg: &LoadedSegment,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let key = ResourceKey::from_url(&seg.media_url);
        let resource = self.fetch.backend().open_resource(&key)?;

        let read_end = (media_offset + buf.len() as u64).min(seg.media_len);
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(bytes_read)
    }
}

impl Source for HlsSource {
    type Error = HlsError;

    #[expect(
        clippy::significant_drop_tightening,
        reason = "lock must be held for condvar wait"
    )]
    fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, HlsError> {
        const MAX_METADATA_MISS_SPINS: usize = 20;
        let mut on_demand_requested = false;
        let mut metadata_miss_count = 0usize;

        // Wait until the range is covered by loaded segments or EOF.
        loop {
            let mut segments = self.shared.segments.lock();

            // Check cancellation (mirrors kithara-storage driver.rs:261)
            if self.shared.cancel.is_cancelled() {
                return Err(StreamError::Source(HlsError::Cancelled));
            }

            // Check if downloader has exited (normally or abnormally).
            // When stopped=true, no more data will be delivered.
            // If the requested range is already loaded, fall through to normal checks.
            // Otherwise, return Eof (past end) or Cancelled (gap / abnormal exit).
            if self.shared.stopped.load(Ordering::Acquire)
                && !segments.is_range_loaded(&range)
                && segments.find_at_offset(range.start).is_none()
            {
                let eof = self.shared.timeline.eof();
                let total = segments.max_end_offset();
                if eof && range.start >= total {
                    return Ok(WaitOutcome::Eof);
                }
                return Err(StreamError::Source(HlsError::Cancelled));
            }

            let eof = self.shared.timeline.eof();
            let total = segments.max_end_offset();
            let has_range_start = segments.find_at_offset(range.start).is_some();

            // Check if range is already loaded (using loaded_ranges from DownloadState)
            if segments.is_range_loaded(&range) {
                return Ok(WaitOutcome::Ready);
            }

            // Fallback check for partial coverage
            if has_range_start {
                return Ok(WaitOutcome::Ready);
            }

            if eof && range.start >= total {
                let expected = self.shared.timeline.total_bytes().unwrap_or(0);
                debug!(
                    range_start = range.start,
                    total_bytes = total,
                    expected_total_length = expected,
                    num_entries = segments.num_entries(),
                    had_midstream_switch = self.shared.had_midstream_switch.load(Ordering::Acquire),
                    "wait_range: EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            // On-demand loading: request specific segment ONCE.
            if !on_demand_requested && !segments.is_range_loaded(&range) {
                if self.shared.had_midstream_switch.load(Ordering::Acquire) {
                    // After variant switch, metadata offsets don't match storage.
                    // Just wake the sequential downloader — it will fill the gap.
                    drop(segments);
                    self.shared.reader_advanced.notify_one();
                    on_demand_requested = true;
                    segments = self.shared.segments.lock();
                } else {
                    let current_variant = self.shared.current_variant_index.load(Ordering::Acquire);

                    drop(segments);
                    if let Some(segment_index) = self
                        .playlist_state
                        .find_segment_at_offset(current_variant, range.start)
                    {
                        metadata_miss_count = 0;
                        debug!(
                            variant = current_variant,
                            segment_index,
                            offset = range.start,
                            "wait_range: requesting on-demand segment load"
                        );

                        self.shared.segment_requests.push(SegmentRequest {
                            segment_index,
                            variant: current_variant,
                            seek_epoch: self.shared.seek_epoch.load(Ordering::Acquire),
                        });

                        self.shared.reader_advanced.notify_one();
                        on_demand_requested = true;
                    } else if let Some(segment_index) =
                        self.fallback_segment_index_for_offset(current_variant, range.start)
                    {
                        metadata_miss_count = 0;
                        debug!(
                            variant = current_variant,
                            segment_index,
                            offset = range.start,
                            "wait_range: metadata miss fallback segment load"
                        );

                        self.shared.segment_requests.push(SegmentRequest {
                            segment_index,
                            variant: current_variant,
                            seek_epoch: self.shared.seek_epoch.load(Ordering::Acquire),
                        });

                        self.shared.reader_advanced.notify_one();
                        on_demand_requested = true;
                    } else {
                        metadata_miss_count = metadata_miss_count.saturating_add(1);
                        let seek_epoch = self.shared.seek_epoch.load(Ordering::Acquire);
                        debug!(
                            offset = range.start,
                            variant = current_variant,
                            seek_epoch,
                            metadata_miss_count,
                            "wait_range: no metadata to find segment"
                        );
                        self.bus.publish(HlsEvent::SeekMetadataMiss {
                            seek_epoch,
                            offset: range.start,
                            variant: current_variant,
                        });
                        self.shared.reader_advanced.notify_one();

                        if metadata_miss_count >= MAX_METADATA_MISS_SPINS {
                            let error = format!(
                                "seek metadata miss: offset={} variant={} epoch={} misses={}",
                                range.start, current_variant, seek_epoch, metadata_miss_count
                            );
                            self.bus.publish(HlsEvent::Error {
                                error: error.clone(),
                                recoverable: false,
                            });
                            return Err(StreamError::Source(HlsError::SegmentNotFound(error)));
                        }
                    }
                    segments = self.shared.segments.lock();
                }
            } else if !on_demand_requested {
                self.shared.reader_advanced.notify_one();
            }

            // Wait for new data with a 50ms timeout as a safety net against
            // missed notifications. On wasm32 the timeout is ignored by the
            // platform Condvar (Instant::now panics), but notifications from
            // the downloader are reliable.
            self.shared
                .condvar
                .wait_for(&mut segments, Duration::from_millis(50));
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, HlsError> {
        let seg = {
            let segments = self.shared.segments.lock();
            segments.find_at_offset(offset).cloned()
        };

        let Some(seg) = seg else {
            return Ok(0);
        };

        #[expect(clippy::cast_possible_truncation, reason = "segment index fits in u32")]
        self.shared
            .current_segment_index
            .store(seg.segment_index as u32, Ordering::Relaxed);
        self.shared
            .current_variant_index
            .store(seg.variant, Ordering::Relaxed);

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
                return Ok(0);
            }
        }

        let bytes = self
            .read_from_entry(&seg, offset, buf)
            .map_err(StreamError::Source)?;

        if bytes > 0 {
            let new_pos = offset.saturating_add(bytes as u64);
            self.shared.timeline.set_byte_position(new_pos);
            self.shared.reader_advanced.notify_one();

            let total = self.shared.segments.lock().max_end_offset();
            self.bus.publish(HlsEvent::ByteProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        self.shared.timeline.total_bytes()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let hinted_variant = self.shared.current_variant_index.load(Ordering::Acquire);
        let reader_variant = self.current_loaded_segment().map(|seg| seg.variant);
        let has_hinted_variant = self
            .shared
            .segments
            .lock()
            .first_segment_of_variant(hinted_variant)
            .is_some();
        let variant = match reader_variant {
            Some(reader) if reader == hinted_variant => reader,
            Some(reader) if self.variant_fence.is_some() && has_hinted_variant => hinted_variant,
            Some(reader) => reader,
            None if has_hinted_variant => hinted_variant,
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
        let segments = self.shared.segments.lock();
        let current_variant = self.shared.current_variant_index.load(Ordering::Acquire);
        // Find the first segment of the current variant.
        // This is where init data (ftyp/moov) lives after an ABR switch.
        if let Some(seg) = segments.first_segment_of_variant(current_variant) {
            return Some(seg.byte_offset..seg.end_offset());
        }

        let reader_offset = self.shared.timeline.byte_position();
        let fallback_variant = segments
            .find_at_offset(reader_offset)
            .or_else(|| segments.last())
            .map(|seg| seg.variant)?;

        segments
            .first_segment_of_variant(fallback_variant)
            .map(|seg| seg.byte_offset..seg.end_offset())
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }

    fn set_seek_epoch(&mut self, seek_epoch: u64) {
        self.shared.seek_epoch.store(seek_epoch, Ordering::Release);
        self.shared.timeline.set_eof(false);
        self.shared.timeline.set_download_position(0);
        while self.shared.segment_requests.pop().is_some() {}
        {
            let mut segments = self.shared.segments.lock();
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
        let seek_epoch = self.shared.seek_epoch.load(Ordering::Acquire);

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
            .current_variant_index
            .store(variant, Ordering::Relaxed);
        self.shared
            .current_segment_index
            .store(anchor.segment_index.unwrap_or(0), Ordering::Relaxed);

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
    #[cfg(not(target_arch = "wasm32"))] coverage_index: Option<
        Arc<kithara_assets::CoverageIndex<kithara_storage::MmapResource>>,
    >,
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
    let timeline = Timeline::new();
    timeline.set_total_duration(playlist_state.track_duration());
    let shared = Arc::new(SharedSegments::new(
        cancel,
        Arc::clone(&playlist_state),
        timeline,
    ));

    let downloader = HlsDownloader {
        active_seek_epoch: 0,
        io: HlsIo::new(Arc::clone(&fetch)),
        fetch: Arc::clone(&fetch),
        playlist_state: Arc::clone(&playlist_state),
        current_segment_index: 0,
        last_committed_variant: None,
        sent_init_for_variant: HashSet::new(),
        abr,
        byte_offset: 0,
        shared: Arc::clone(&shared),
        bus: bus.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        prefetch_count: config.download_batch_size.max(1),
        #[cfg(not(target_arch = "wasm32"))]
        coverage_index,
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
    pub fn segment_index_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.shared.current_segment_index)
    }

    /// Handle to current variant index atomic.
    pub fn variant_index_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.shared.current_variant_index)
    }
}

#[cfg(test)]
mod tests {
    use std::{ops::Range, sync::Arc, time::Duration};

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::DecryptContext;
    use kithara_events::Event;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_stream::AudioCodec;
    use rstest::rstest;
    use url::Url;

    use super::*;
    use crate::fetch::FetchManager;
    use crate::parsing::{VariantId, VariantStream};
    use crate::playlist::{SegmentState, VariantSizeMap, VariantState};

    #[derive(Clone, Copy)]
    enum WaitRangeUnblock {
        Cancel,
        Stopped,
    }

    /// Create a dummy PlaylistState for tests (no real playlists needed).
    fn dummy_playlist_state() -> Arc<PlaylistState> {
        Arc::new(PlaylistState::new(vec![]))
    }

    /// Create a minimal `HlsSource` for testing `wait_range` behavior.
    fn make_test_source(shared: Arc<SharedSegments>, cancel: CancellationToken) -> HlsSource {
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
        let fetch = Arc::new(FetchManager::new(backend, net, cancel));
        let playlist_state = Arc::clone(&shared.playlist_state);
        HlsSource {
            fetch,
            shared,
            playlist_state,
            bus: EventBus::new(16),
            variant_fence: None,
            _backend: None,
        }
    }

    fn make_loaded_segment(
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
        media_len: u64,
    ) -> LoadedSegment {
        LoadedSegment {
            variant,
            segment_index,
            byte_offset,
            init_len: 0,
            media_len,
            init_url: None,
            media_url: Url::parse("https://example.com/seg").unwrap(),
        }
    }

    fn make_variant_state_with_codec(
        id: usize,
        count: usize,
        codec: Option<AudioCodec>,
    ) -> VariantState {
        let base = Url::parse("https://example.com/").unwrap();
        let segments = (0..count)
            .map(|index| SegmentState {
                index,
                url: base
                    .join(&format!("v{id}/seg-{index}.m4s"))
                    .expect("valid segment URL"),
                duration: Duration::from_secs(4),
                key: None,
            })
            .collect();

        VariantState {
            id,
            uri: base
                .join(&format!("v{id}.m3u8"))
                .expect("valid playlist URL"),
            bandwidth: Some(128_000),
            codec,
            container: None,
            init_url: None,
            segments,
            size_map: None,
        }
    }

    fn make_variant_state(id: usize, count: usize) -> VariantState {
        make_variant_state_with_codec(id, count, None)
    }

    fn uniform_size_map(segments: usize, segment_size: u64) -> VariantSizeMap {
        let offsets: Vec<u64> = (0..segments).map(|i| i as u64 * segment_size).collect();
        VariantSizeMap {
            init_size: 0,
            segment_sizes: vec![segment_size; segments],
            offsets,
            total: segments as u64 * segment_size,
        }
    }

    fn playlist_state_with_size_maps() -> Arc<PlaylistState> {
        let state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, 24),
            make_variant_state(1, 24),
        ]));
        state.set_size_map(0, uniform_size_map(24, 100));
        state.set_size_map(1, uniform_size_map(24, 100));
        state
    }

    fn playlist_state_with_codecs(
        first_codec: Option<AudioCodec>,
        second_codec: Option<AudioCodec>,
    ) -> Arc<PlaylistState> {
        Arc::new(PlaylistState::new(vec![
            make_variant_state_with_codec(0, 4, first_codec),
            make_variant_state_with_codec(1, 4, second_codec),
        ]))
    }

    fn playlist_state_without_size_map() -> Arc<PlaylistState> {
        Arc::new(PlaylistState::new(vec![make_variant_state(0, 24)]))
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

    async fn wait_range_and_take_request(
        shared: Arc<SharedSegments>,
        mut source: HlsSource,
        range: Range<u64>,
    ) -> SegmentRequest {
        let handle = tokio::task::spawn_blocking(move || source.wait_range(range));

        let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
        let request = loop {
            if let Some(request) = shared.segment_requests.pop() {
                break request;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("expected on-demand segment request");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        shared.cancel.cancel();
        shared.condvar.notify_all();
        let _ = tokio::time::timeout(Duration::from_millis(400), handle)
            .await
            .expect("wait_range task should complete")
            .expect("wait_range task should not panic");

        request
    }

    #[tokio::test]
    async fn seek_time_anchor_resolves_segment_and_queues_request() {
        let cancel = CancellationToken::new();
        let playlist_state = playlist_state_with_size_maps();
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            Arc::clone(&playlist_state),
            Timeline::new(),
        ));
        shared.seek_epoch.store(9, Ordering::Release);
        shared.current_variant_index.store(0, Ordering::Relaxed);

        let mut source = make_test_source(Arc::clone(&shared), cancel);
        let anchor = Source::seek_time_anchor(&mut source, Duration::from_millis(8_500))
            .expect("seek anchor resolution should succeed")
            .expect("HLS source should resolve anchor");

        assert_eq!(anchor.segment_index, Some(2));
        assert_eq!(anchor.variant_index, Some(0));
        assert_eq!(anchor.byte_offset, 200);
        assert_eq!(anchor.segment_start, Duration::from_secs(8));
        assert_eq!(anchor.segment_end, Some(Duration::from_secs(12)));

        let req = shared
            .segment_requests
            .pop()
            .expect("anchor seek should enqueue request");
        assert_eq!(req.variant, 0);
        assert_eq!(req.segment_index, 2);
        assert_eq!(req.seek_epoch, 9);
    }

    #[test]
    fn media_info_uses_reader_offset_variant_instead_of_last_loaded_segment() {
        let cancel = CancellationToken::new();
        let playlist_state =
            playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::Flac));
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            Arc::clone(&playlist_state),
            Timeline::new(),
        ));
        {
            let mut segments = shared.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
            segments.push(make_loaded_segment(1, 0, 100, 100));
        }

        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        shared.timeline.set_byte_position(0);
        let info_at_start = Source::media_info(&source).expect("media info at start");
        assert_eq!(info_at_start.codec, Some(AudioCodec::AacLc));

        shared.timeline.set_byte_position(100);
        let info_after_switch = Source::media_info(&source).expect("media info at switch");
        assert_eq!(info_after_switch.codec, Some(AudioCodec::Flac));

        // Variant fence path can expose the target variant before reader_offset advances.
        shared.timeline.set_byte_position(0);
        shared.current_variant_index.store(1, Ordering::Release);
        source.variant_fence = Some(0);
        let hinted_info = Source::media_info(&source).expect("media info from hinted variant");
        assert_eq!(hinted_info.codec, Some(AudioCodec::Flac));
    }

    #[test]
    fn current_segment_range_uses_reader_offset_not_last_segment() {
        let cancel = CancellationToken::new();
        let playlist_state = playlist_state_with_codecs(None, None);
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            Arc::clone(&playlist_state),
            Timeline::new(),
        ));
        {
            let mut segments = shared.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
            segments.push(make_loaded_segment(0, 1, 100, 100));
        }

        let source = make_test_source(Arc::clone(&shared), cancel);

        shared.timeline.set_byte_position(0);
        assert_eq!(Source::current_segment_range(&source), Some(0..100));

        shared.timeline.set_byte_position(120);
        assert_eq!(Source::current_segment_range(&source), Some(100..200));
    }

    #[test]
    fn set_seek_epoch_flushes_playback_segments() {
        let cancel = CancellationToken::new();
        let playlist_state = playlist_state_with_codecs(None, None);
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            Arc::clone(&playlist_state),
            Timeline::new(),
        ));
        {
            let mut segments = shared.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
            segments.push(make_loaded_segment(0, 1, 100, 100));
        }
        shared.timeline.set_eof(true);
        shared.timeline.set_download_position(200);
        shared.seek_epoch.store(3, Ordering::Release);

        let mut source = make_test_source(Arc::clone(&shared), cancel);
        Source::set_seek_epoch(&mut source, 4);

        assert_eq!(shared.seek_epoch.load(Ordering::Acquire), 4);
        assert!(!shared.timeline.eof());
        assert_eq!(shared.timeline.download_position(), 0);
        assert_eq!(shared.current_segment_index.load(Ordering::Acquire), 0);
        assert_eq!(shared.segments.lock().num_entries(), 0);
    }

    #[test]
    fn codec_fence_allows_cross_variant_reads_when_codec_matches() {
        let cancel = CancellationToken::new();
        let playlist_state =
            playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::AacLc));
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            playlist_state,
            Timeline::new(),
        ));
        let source = make_test_source(shared, cancel);

        assert!(source.can_cross_variant_without_reset(0, 1));
    }

    #[test]
    fn codec_fence_blocks_cross_variant_reads_when_codec_changes() {
        let cancel = CancellationToken::new();
        let playlist_state =
            playlist_state_with_codecs(Some(AudioCodec::AacLc), Some(AudioCodec::Flac));
        let shared = Arc::new(SharedSegments::new(
            cancel.clone(),
            playlist_state,
            Timeline::new(),
        ));
        let source = make_test_source(shared, cancel);

        assert!(!source.can_cross_variant_without_reset(0, 1));
    }

    #[test]
    fn build_pair_seeds_timeline_total_duration_from_playlist() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, 4),
            make_variant_state(1, 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = crate::config::HlsConfig {
            cancel: Some(cancel),
            ..crate::config::HlsConfig::default()
        };

        #[cfg(not(target_arch = "wasm32"))]
        let (_, source) = build_pair(
            fetch,
            &variants,
            &config,
            None,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );
        #[cfg(target_arch = "wasm32")]
        let (_, source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        assert_eq!(
            Source::timeline(&source).total_duration(),
            Some(Duration::from_secs(16))
        );
    }

    #[test]
    fn test_fence_at_removes_stale_entries() {
        let mut state = DownloadState::new();

        // V0 entries: 0..100, 100..200, 200..300
        state.push(make_loaded_segment(0, 0, 0, 100));
        state.push(make_loaded_segment(0, 1, 100, 100));
        state.push(make_loaded_segment(0, 2, 200, 100));

        // V3 entries: 300..400
        state.push(make_loaded_segment(3, 0, 300, 100));

        assert_eq!(state.num_entries(), 4);
        assert!(state.is_segment_loaded(0, 2));

        // Fence at offset 200, keep V3.
        // V0 entries at offset >= 200 should be removed (entry at 200..300).
        // V0 entries before offset 200 should be kept (entries at 0..100, 100..200).
        // V3 entries should be kept regardless.
        state.fence_at(200, 3);

        assert_eq!(state.num_entries(), 3);

        // V0 entries before fence are kept
        assert!(state.is_segment_loaded(0, 0));
        assert!(state.is_segment_loaded(0, 1));

        // V0 entry at/past fence is removed
        assert!(!state.is_segment_loaded(0, 2));

        // V3 entry is kept
        assert!(state.is_segment_loaded(3, 0));

        // loaded_ranges rebuilt correctly
        assert!(state.is_range_loaded(&(0..200)));
        assert!(!state.is_range_loaded(&(200..300)));
        assert!(state.is_range_loaded(&(300..400)));
    }

    #[test]
    fn test_find_at_offset_after_fence() {
        let mut state = DownloadState::new();

        // V0: 0..100, 100..200
        state.push(make_loaded_segment(0, 0, 0, 100));
        state.push(make_loaded_segment(0, 1, 100, 100));

        // V3: 200..300
        state.push(make_loaded_segment(3, 0, 200, 100));

        // Before fence, V0 entry at 100 is findable
        assert!(state.find_at_offset(150).is_some());
        assert_eq!(state.find_at_offset(150).unwrap().variant, 0);

        // Fence at 100, keep V3
        state.fence_at(100, 3);

        // V0 entry at 100..200 removed -- offset 150 not found
        assert!(state.find_at_offset(150).is_none());

        // V0 entry at 0..100 still there
        assert!(state.find_at_offset(50).is_some());
        assert_eq!(state.find_at_offset(50).unwrap().variant, 0);

        // V3 entry at 200..300 still there
        assert!(state.find_at_offset(250).is_some());
        assert_eq!(state.find_at_offset(250).unwrap().variant, 3);
    }

    #[test]
    fn test_wait_range_blocks_after_fence() {
        let mut state = DownloadState::new();

        // V0: 0..100, 100..200
        state.push(make_loaded_segment(0, 0, 0, 100));
        state.push(make_loaded_segment(0, 1, 100, 100));

        // V3: 200..300
        state.push(make_loaded_segment(3, 0, 200, 100));

        // Range 100..200 is loaded (V0 entry)
        assert!(state.is_range_loaded(&(100..200)));

        // Fence at 100, keep V3
        state.fence_at(100, 3);

        // Range 100..200 is no longer loaded (V0 entry removed)
        assert!(!state.is_range_loaded(&(100..200)));

        // Range 0..100 still loaded (V0 entry before fence)
        assert!(state.is_range_loaded(&(0..100)));

        // Range 200..300 still loaded (V3 entry)
        assert!(state.is_range_loaded(&(200..300)));
    }

    #[test]
    fn test_cumulative_offset_after_switch() {
        let mut state = DownloadState::new();

        // Simulate V0 segments 0..13 occupying 0..700
        for i in 0..14 {
            state.push(make_loaded_segment(0, i, i as u64 * 50, 50));
        }

        assert_eq!(state.max_end_offset(), 700);
        assert!(state.is_range_loaded(&(0..700)));

        // Simulate variant switch: fence at 700, keep V3
        state.fence_at(700, 3);

        // V3 seg 14 placed at cumulative offset 700 (not metadata offset)
        state.push(make_loaded_segment(3, 14, 700, 200));

        // V3 seg 15 placed contiguously at 900
        state.push(make_loaded_segment(3, 15, 900, 200));

        // Verify contiguous layout with no gaps
        assert!(state.is_range_loaded(&(0..700)));
        assert!(state.is_range_loaded(&(700..900)));
        assert!(state.is_range_loaded(&(900..1100)));
        assert!(state.is_range_loaded(&(0..1100)));

        // No gap between V0 and V3
        assert!(state.find_at_offset(700).is_some());
        assert_eq!(state.find_at_offset(700).unwrap().variant, 3);
        assert_eq!(state.find_at_offset(700).unwrap().segment_index, 14);
    }

    #[test]
    fn test_last_entry_tracks_most_recent_push() {
        let mut state = DownloadState::new();

        assert!(state.last().is_none());

        state.push(make_loaded_segment(0, 0, 0, 100));
        assert_eq!(state.last().unwrap().segment_index, 0);
        assert_eq!(state.last().unwrap().variant, 0);

        state.push(make_loaded_segment(0, 1, 100, 100));
        assert_eq!(state.last().unwrap().segment_index, 1);

        // After variant switch
        state.push(make_loaded_segment(3, 14, 200, 100));
        assert_eq!(state.last().unwrap().variant, 3);
        assert_eq!(state.last().unwrap().segment_index, 14);
    }

    #[test]
    fn test_had_midstream_switch_flag() {
        let ps = dummy_playlist_state();
        let shared = SharedSegments::new(CancellationToken::new(), ps, Timeline::new());
        assert!(!shared.had_midstream_switch.load(Ordering::Acquire));

        shared.had_midstream_switch.store(true, Ordering::Release);
        assert!(shared.had_midstream_switch.load(Ordering::Acquire));
    }

    #[test]
    fn test_max_end_offset() {
        let mut state = DownloadState::new();
        assert_eq!(state.max_end_offset(), 0);

        // Entries from different variants at different offsets
        state.push(make_loaded_segment(0, 0, 0, 100));
        assert_eq!(state.max_end_offset(), 100);

        state.push(make_loaded_segment(0, 1, 100, 200));
        assert_eq!(state.max_end_offset(), 300);

        // V3 entry at higher offset
        state.push(make_loaded_segment(3, 5, 500, 100));
        assert_eq!(state.max_end_offset(), 600);
    }

    // wait_range cancellation tests

    #[rstest]
    #[case(WaitRangeUnblock::Cancel)]
    #[case(WaitRangeUnblock::Stopped)]
    #[tokio::test]
    async fn test_wait_range_unblocks_with_error(#[case] unblock: WaitRangeUnblock) {
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        let shared2 = Arc::clone(&shared);
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let handle = tokio::task::spawn_blocking(move || source.wait_range(0..1024));

        // Give wait_range time to enter the loop
        tokio::time::sleep(Duration::from_millis(20)).await;

        match unblock {
            WaitRangeUnblock::Cancel => cancel.cancel(),
            WaitRangeUnblock::Stopped => {
                shared2.stopped.store(true, Ordering::Release);
                shared2.condvar.notify_all();
            }
        }

        let result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("task should complete within 200ms")
            .expect("task should not panic");

        assert!(
            result.is_err(),
            "wait_range should return cancellation error"
        );
    }

    #[tokio::test]
    async fn test_wait_range_returns_ready_when_data_pushed() {
        // Normal scenario: push segment data, wait_range returns Ready.
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        let shared2 = Arc::clone(&shared);
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let handle = tokio::task::spawn_blocking(move || source.wait_range(0..100));

        // Push a segment covering 0..100
        tokio::time::sleep(Duration::from_millis(20)).await;
        {
            let mut segments = shared2.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
        }
        shared2.condvar.notify_all();

        let result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("task should complete within 200ms")
            .expect("task should not panic");

        assert!(matches!(result, Ok(WaitOutcome::Ready)));
    }

    #[tokio::test]
    async fn test_wait_range_eof_when_stopped_and_past_end() {
        // Downloader stopped + eof -- wait_range at past-end offset returns Eof.
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        let shared2 = Arc::clone(&shared);

        // Push one segment
        {
            let mut segments = shared2.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
        }
        // Mark eof + stopped
        shared2.timeline.set_eof(true);
        shared2.stopped.store(true, Ordering::Release);

        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let result = source.wait_range(100..200);
        assert!(matches!(result, Ok(WaitOutcome::Eof)));
    }

    #[tokio::test]
    async fn test_wait_range_uses_active_variant_for_seek_request() {
        let cancel = CancellationToken::new();
        let ps = playlist_state_with_size_maps();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        let source = make_test_source(Arc::clone(&shared), cancel.clone());

        // Last pushed segment is from variant 1, but active playback variant is 0.
        {
            let mut segments = shared.segments.lock();
            segments.push(make_loaded_segment(1, 5, 500, 100));
        }
        shared.current_variant_index.store(0, Ordering::Release);

        let request = wait_range_and_take_request(Arc::clone(&shared), source, 150..170).await;
        assert_eq!(request.variant, 0);
        assert_eq!(request.segment_index, 1);
    }

    #[tokio::test]
    async fn test_wait_range_without_size_map_uses_segment_zero_fallback() {
        let cancel = CancellationToken::new();
        let ps = playlist_state_without_size_map();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        shared.current_variant_index.store(0, Ordering::Release);
        let source = make_test_source(Arc::clone(&shared), cancel.clone());

        let request = wait_range_and_take_request(Arc::clone(&shared), source, 0..128).await;
        assert_eq!(request.variant, 0);
        assert_eq!(request.segment_index, 0);
    }

    #[tokio::test]
    async fn test_wait_range_missing_metadata_fails_fast_with_diagnostic() {
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps, Timeline::new()));
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());
        let mut events = source.bus.subscribe();

        let handle = tokio::task::spawn_blocking(move || source.wait_range(1024..2048));
        let mut saw_metadata_miss = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(50), events.recv()).await {
                Ok(Ok(Event::Hls(HlsEvent::SeekMetadataMiss { .. }))) => {
                    saw_metadata_miss = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => {}
                Err(_) => {}
            }
        }

        let result = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("wait_range should fail fast")
            .expect("wait_range task should not panic");

        match result {
            Err(StreamError::Source(HlsError::SegmentNotFound(message))) => {
                assert!(
                    message.contains("seek metadata miss"),
                    "unexpected error message: {message}"
                );
            }
            other => panic!("unexpected wait_range result: {other:?}"),
        }

        assert!(saw_metadata_miss, "expected SeekMetadataMiss diagnostic");
    }
}
