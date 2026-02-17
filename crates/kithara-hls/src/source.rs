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
    time::Duration,
};

use crossbeam_queue::SegQueue;
use kithara_abr::Variant;
use kithara_assets::ResourceKey;
use kithara_events::{EventBus, HlsEvent};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{MediaInfo, Source, StreamError, StreamResult};
use parking_lot::{Condvar, Mutex};
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
}

/// Shared state between `HlsDownloader` and `HlsSource`.
pub(crate) struct SharedSegments {
    pub(crate) segments: Mutex<DownloadState>,
    /// Downloader → Source: new segment available (for sync blocking in Source).
    pub(crate) condvar: Condvar,
    pub(crate) eof: AtomicBool,
    /// Current reader byte offset (updated by Source on every `read_at`).
    pub(crate) reader_offset: AtomicU64,
    /// Source → Downloader: reader advanced, may resume downloading.
    pub(crate) reader_advanced: Notify,
    /// Segment load requests (on-demand from seek or sequential).
    pub(crate) segment_requests: SegQueue<SegmentRequest>,
    /// Parsed playlist data (variant info, segment URLs, size maps).
    pub(crate) playlist_state: Arc<PlaylistState>,
    /// Expected total length of current variant's theoretical stream.
    /// Used for seek bounds checking by Symphonia.
    pub(crate) expected_total_length: AtomicU64,
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
}

impl SharedSegments {
    pub(crate) fn new(cancel: CancellationToken, playlist_state: Arc<PlaylistState>) -> Self {
        Self {
            segments: Mutex::new(DownloadState::new()),
            condvar: Condvar::new(),
            eof: AtomicBool::new(false),
            reader_offset: AtomicU64::new(0),
            reader_advanced: Notify::new(),
            segment_requests: SegQueue::new(),
            playlist_state,
            expected_total_length: AtomicU64::new(0),
            had_midstream_switch: AtomicBool::new(false),
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

    fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, HlsError> {
        let mut on_demand_requested = false;

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
                let eof = self.shared.eof.load(Ordering::Acquire);
                let total = segments.max_end_offset();
                if eof && range.start >= total {
                    return Ok(WaitOutcome::Eof);
                }
                return Err(StreamError::Source(HlsError::Cancelled));
            }

            let eof = self.shared.eof.load(Ordering::Acquire);
            let total = segments.max_end_offset();

            // Check if range is already loaded (using loaded_ranges from DownloadState)
            if segments.is_range_loaded(&range) {
                return Ok(WaitOutcome::Ready);
            }

            // Fallback check for partial coverage
            if segments.find_at_offset(range.start).is_some() {
                return Ok(WaitOutcome::Ready);
            }

            if eof && range.start >= total {
                let expected = self.shared.expected_total_length.load(Ordering::Acquire);
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

            // On-demand loading: request specific segment ONCE
            if !on_demand_requested && !segments.is_range_loaded(&range) {
                if self.shared.had_midstream_switch.load(Ordering::Acquire) {
                    // After variant switch, metadata offsets don't match storage.
                    // Just wake the sequential downloader — it will fill the gap.
                    drop(segments);
                    self.shared.reader_advanced.notify_one();
                    on_demand_requested = true;
                    segments = self.shared.segments.lock();
                } else {
                    let current_variant = segments.last().map_or(0, |s| s.variant);

                    drop(segments);
                    if let Some(segment_index) = self
                        .playlist_state
                        .find_segment_at_offset(current_variant, range.start)
                    {
                        debug!(
                            variant = current_variant,
                            segment_index,
                            offset = range.start,
                            "wait_range: requesting on-demand segment load"
                        );

                        // Clear queue (cancel sequential loads, prioritize seek)
                        while self.shared.segment_requests.pop().is_some() {}

                        self.shared.segment_requests.push(SegmentRequest {
                            segment_index,
                            variant: current_variant,
                        });

                        self.shared.reader_advanced.notify_one();
                        on_demand_requested = true;
                    } else {
                        debug!(
                            offset = range.start,
                            variant = current_variant,
                            "wait_range: no metadata to find segment"
                        );
                    }
                    segments = self.shared.segments.lock();
                }
            } else if !on_demand_requested {
                self.shared.reader_advanced.notify_one();
            }

            // Wait with timeout (mirrors kithara-storage driver.rs:283)
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

        // Variant fence: auto-detect on first read, block cross-variant reads.
        if self.variant_fence.is_none() {
            self.variant_fence = Some(seg.variant);
        }
        if let Some(fence) = self.variant_fence
            && seg.variant != fence
        {
            return Ok(0);
        }

        self.shared
            .current_segment_index
            .store(seg.segment_index as u32, Ordering::Relaxed);
        self.shared
            .current_variant_index
            .store(seg.variant, Ordering::Relaxed);

        let bytes = self
            .read_from_entry(&seg, offset, buf)
            .map_err(StreamError::Source)?;

        if bytes > 0 {
            let new_pos = offset.saturating_add(bytes as u64);
            self.shared.reader_offset.store(new_pos, Ordering::Release);
            self.shared.reader_advanced.notify_one();

            let total = self.shared.segments.lock().max_end_offset();
            self.bus.publish(HlsEvent::PlaybackProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        let expected = self.shared.expected_total_length.load(Ordering::Acquire);
        if expected > 0 { Some(expected) } else { None }
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let segments = self.shared.segments.lock();
        let last = segments.last()?;
        let codec = self.playlist_state.variant_codec(last.variant);
        let container = self.playlist_state.variant_container(last.variant);
        Some(MediaInfo::new(codec, container).with_variant_index(last.variant as u32))
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        let segments = self.shared.segments.lock();
        segments.last().map(|seg| seg.byte_offset..seg.end_offset())
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let segments = self.shared.segments.lock();
        let last = segments.last()?;
        // Find the first segment of the current variant.
        // This is where init data (ftyp/moov) lives after an ABR switch.
        segments
            .first_segment_of_variant(last.variant)
            .map(|seg| seg.byte_offset..seg.end_offset())
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }
}

/// Build an `HlsDownloader` + `HlsSource` pair from config.
pub fn build_pair(
    fetch: Arc<DefaultFetchManager>,
    variants: &[crate::parsing::VariantStream],
    config: &crate::config::HlsConfig,
    coverage_index: Option<Arc<kithara_assets::CoverageIndex<kithara_storage::MmapResource>>>,
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
    let shared = Arc::new(SharedSegments::new(cancel, Arc::clone(&playlist_state)));

    let downloader = HlsDownloader {
        io: HlsIo::new(Arc::clone(&fetch)),
        fetch: Arc::clone(&fetch),
        playlist_state: Arc::clone(&playlist_state),
        current_segment_index: 0,
        sent_init_for_variant: HashSet::new(),
        abr,
        byte_offset: 0,
        shared: Arc::clone(&shared),
        bus: bus.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        prefetch_count: config.download_batch_size.max(1),
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
    use std::{sync::Arc, time::Duration};

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::DecryptContext;
    use kithara_net::{HttpClient, NetOptions};
    use url::Url;

    use super::*;
    use crate::fetch::FetchManager;

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
        let shared = SharedSegments::new(CancellationToken::new(), ps);
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

    // --- wait_range cancellation tests ---

    #[tokio::test]
    async fn test_wait_range_cancel_unblocks() {
        // Cancel token should unblock wait_range within 100ms.
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps));
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let handle = tokio::task::spawn_blocking(move || source.wait_range(0..1024));

        // Give wait_range time to enter the loop
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("task should complete within 200ms")
            .expect("task should not panic");

        assert!(result.is_err(), "wait_range should return Cancelled error");
    }

    #[tokio::test]
    async fn test_wait_range_stopped_downloader_unblocks() {
        // Downloader stop (via stopped flag + condvar) should unblock wait_range.
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps));
        let shared2 = Arc::clone(&shared);
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let handle = tokio::task::spawn_blocking(move || source.wait_range(0..1024));

        // Give wait_range time to enter the loop
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Simulate downloader drop
        shared2.stopped.store(true, Ordering::Release);
        shared2.condvar.notify_all();

        let result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("task should complete within 200ms")
            .expect("task should not panic");

        assert!(
            result.is_err(),
            "wait_range should return error when downloader stopped"
        );
    }

    #[tokio::test]
    async fn test_wait_range_returns_ready_when_data_pushed() {
        // Normal scenario: push segment data, wait_range returns Ready.
        let cancel = CancellationToken::new();
        let ps = dummy_playlist_state();
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps));
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
        let shared = Arc::new(SharedSegments::new(cancel.clone(), ps));
        let shared2 = Arc::clone(&shared);

        // Push one segment
        {
            let mut segments = shared2.segments.lock();
            segments.push(make_loaded_segment(0, 0, 0, 100));
        }
        // Mark eof + stopped
        shared2.eof.store(true, Ordering::Release);
        shared2.stopped.store(true, Ordering::Release);

        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let result = source.wait_range(100..200);
        assert!(matches!(result, Ok(WaitOutcome::Eof)));
    }
}
