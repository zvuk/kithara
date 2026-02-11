//! HLS source: random-access reading from loaded HLS segments.
//!
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! Shared state with `HlsDownloader` (in `downloader.rs`) via `SharedSegments`.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use crossbeam_queue::SegQueue;
use kithara_abr::Variant;
use kithara_assets::ResourceKey;
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{AudioCodec, MediaInfo, Source, StreamError, StreamResult};
use parking_lot::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    downloader::{HlsDownloader, HlsIo, SegmentMetadata},
    events::HlsEvent,
    fetch::{DefaultFetchManager, SegmentMeta},
    parsing::VariantStream,
};

/// Request to load a specific segment (on-demand or sequential).
#[derive(Debug, Clone)]
pub(crate) struct SegmentRequest {
    pub(crate) segment_index: usize,
    pub(crate) variant: usize,
}

/// Entry tracking a loaded segment in the virtual stream.
#[derive(Debug, Clone)]
pub(crate) struct SegmentEntry {
    pub(crate) byte_offset: u64,
    pub(crate) codec: Option<AudioCodec>,
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<Url>,
    /// Segment metadata from the fetch layer (url, len, variant, container).
    pub(crate) meta: SegmentMeta,
}

impl SegmentEntry {
    /// Media segment index in the playlist.
    pub(crate) fn segment_index(&self) -> usize {
        self.meta.segment_type.media_index().unwrap_or(0)
    }

    pub(crate) fn total_len(&self) -> u64 {
        self.init_len + self.meta.len
    }

    pub(crate) fn end_offset(&self) -> u64 {
        self.byte_offset + self.total_len()
    }

    fn contains(&self, offset: u64) -> bool {
        offset >= self.byte_offset && offset < self.end_offset()
    }
}

/// Index of loaded segments.
#[derive(Debug)]
pub(crate) struct SegmentIndex {
    pub(crate) entries: HashMap<(usize, usize), SegmentEntry>,
    /// Expected total length of current variant's theoretical stream.
    /// Used for seek bounds checking by Symphonia.
    pub(crate) expected_total_length: u64,
    /// True after a mid-stream variant switch. On-demand loading should
    /// just wake the sequential downloader instead of using metadata lookups.
    pub(crate) had_midstream_switch: bool,
    /// Key of the most recently pushed entry (for `last()`, `media_info`, `segment_range`).
    last_entry_key: Option<(usize, usize)>,
    /// Byte ranges that have been loaded (reflects actual data on disk).
    loaded_ranges: RangeSet<u64>,
}

impl Default for SegmentIndex {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            expected_total_length: 0,
            had_midstream_switch: false,
            last_entry_key: None,
            loaded_ranges: RangeSet::new(),
        }
    }
}

impl SegmentIndex {
    fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, entry: SegmentEntry) {
        let end = entry.end_offset();
        let range = entry.byte_offset..end;
        self.loaded_ranges.insert(range);
        let key = (entry.meta.variant, entry.segment_index());
        if end > self.expected_total_length {
            self.expected_total_length = end;
        }
        debug!(
            variant = entry.meta.variant,
            segment_index = entry.segment_index(),
            byte_offset = entry.byte_offset,
            end,
            "segment_index::push"
        );
        self.last_entry_key = Some(key);
        self.entries.insert(key, entry);
    }

    pub(crate) fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
        self.entries.contains_key(&(variant, segment_index))
    }

    pub(crate) fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        !self.loaded_ranges.gaps(range).any(|_| true)
    }

    pub(crate) fn last(&self) -> Option<&SegmentEntry> {
        self.last_entry_key.and_then(|key| self.entries.get(&key))
    }

    pub(crate) fn find_at_offset(&self, offset: u64) -> Option<&SegmentEntry> {
        self.entries.values().find(|e| e.contains(offset))
    }

    /// Find the first segment of the given variant (by lowest `byte_offset`).
    ///
    /// Used to find the start of a new variant after ABR switch — this is where
    /// init data (ftyp/moov) lives for the new variant.
    fn first_segment_of_variant(&self, variant: usize) -> Option<&SegmentEntry> {
        self.entries
            .values()
            .filter(|e| e.meta.variant == variant)
            .min_by_key(|e| e.byte_offset)
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(SegmentEntry::end_offset)
            .max()
            .unwrap_or(0)
    }

    /// Remove all entries from other variants at or past the fence offset.
    /// Rebuilds `loaded_ranges` from remaining entries.
    pub(crate) fn fence_at(&mut self, offset: u64, keep_variant: usize) {
        self.entries
            .retain(|_, e| e.byte_offset < offset || e.meta.variant == keep_variant);

        // Rebuild loaded_ranges from remaining entries
        self.loaded_ranges.clear();
        for entry in self.entries.values() {
            self.loaded_ranges
                .insert(entry.byte_offset..entry.end_offset());
        }

        debug!(
            offset,
            keep_variant,
            remaining = self.entries.len(),
            "fence_at"
        );
    }
}

/// Shared state between `HlsDownloader` and `HlsSource`.
pub(crate) struct SharedSegments {
    pub(crate) segments: Mutex<SegmentIndex>,
    /// Downloader → Source: new segment available (for sync blocking in Source).
    pub(crate) condvar: Condvar,
    pub(crate) eof: AtomicBool,
    /// Current reader byte offset (updated by Source on every `read_at`).
    pub(crate) reader_offset: AtomicU64,
    /// Source → Downloader: reader advanced, may resume downloading.
    pub(crate) reader_advanced: Notify,
    /// Segment load requests (on-demand from seek or sequential).
    pub(crate) segment_requests: SegQueue<SegmentRequest>,
    /// Per-variant segment metadata for `byte_offset` → `segment_index` mapping.
    pub(crate) variant_metadata: Mutex<HashMap<usize, Vec<SegmentMetadata>>>,
    /// Cancellation token for interrupting `wait_range`.
    pub(crate) cancel: CancellationToken,
    /// Downloader has exited (normally or with error).
    pub(crate) stopped: AtomicBool,
}

impl SharedSegments {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        Self {
            segments: Mutex::new(SegmentIndex::new()),
            condvar: Condvar::new(),
            eof: AtomicBool::new(false),
            reader_offset: AtomicU64::new(0),
            reader_advanced: Notify::new(),
            segment_requests: SegQueue::new(),
            variant_metadata: Mutex::new(HashMap::new()),
            cancel,
            stopped: AtomicBool::new(false),
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
    events_tx: Option<broadcast::Sender<HlsEvent>>,
    /// Variant fence: auto-detected on first read, blocks cross-variant reads.
    variant_fence: Option<usize>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    _backend: Option<kithara_stream::Backend>,
}

impl HlsSource {
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Find segment index for a given byte offset using variant metadata.
    /// Returns `segment_index` for the segment containing the offset.
    fn find_segment_for_offset(&self, variant: usize, offset: u64) -> Option<usize> {
        let metadata_map = self.shared.variant_metadata.lock();
        let metadata = metadata_map.get(&variant)?;

        metadata
            .iter()
            .position(|seg| offset >= seg.byte_offset && offset < seg.byte_offset + seg.size)
    }

    /// Read from a segment entry.
    fn read_from_entry(
        &self,
        entry: &SegmentEntry,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let local_offset = offset - entry.byte_offset;

        if local_offset < entry.init_len {
            let Some(ref init_url) = entry.init_url else {
                return Ok(0);
            };

            let key = ResourceKey::from_url(init_url);
            let resource = self.fetch.backend().open_resource(&key)?;

            let read_end = (local_offset + buf.len() as u64).min(entry.init_len);
            resource.wait_range(local_offset..read_end)?;

            let available = (entry.init_len - local_offset) as usize;
            let to_read = buf.len().min(available);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read])?;

            if bytes_from_init < buf.len() && entry.meta.len > 0 {
                let remaining = &mut buf[bytes_from_init..];
                let bytes_from_media = self.read_media_segment(entry, 0, remaining)?;
                Ok(bytes_from_init + bytes_from_media)
            } else {
                Ok(bytes_from_init)
            }
        } else {
            let media_offset = local_offset - entry.init_len;
            self.read_media_segment(entry, media_offset, buf)
        }
    }

    fn read_media_segment(
        &self,
        entry: &SegmentEntry,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let key = ResourceKey::from_url(&entry.meta.url);
        let resource = self.fetch.backend().open_resource(&key)?;

        let read_end = (media_offset + buf.len() as u64).min(entry.meta.len);
        resource.wait_range(media_offset..read_end)?;

        let bytes_read = resource.read_at(media_offset, buf)?;
        Ok(bytes_read)
    }
}

impl Source for HlsSource {
    type Item = u8;
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
                let total = segments.total_bytes();
                if eof && range.start >= total {
                    return Ok(WaitOutcome::Eof);
                }
                return Err(StreamError::Source(HlsError::Cancelled));
            }

            let eof = self.shared.eof.load(Ordering::Acquire);
            let total = segments.total_bytes();

            // Check if range is already loaded (using loaded_ranges from SegmentIndex)
            if segments.is_range_loaded(&range) {
                return Ok(WaitOutcome::Ready);
            }

            // Fallback check for partial coverage
            if segments.find_at_offset(range.start).is_some() {
                return Ok(WaitOutcome::Ready);
            }

            if eof && range.start >= total {
                let expected = segments.expected_total_length;
                debug!(
                    range_start = range.start,
                    total_bytes = total,
                    expected_total_length = expected,
                    num_entries = segments.entries.len(),
                    had_midstream_switch = segments.had_midstream_switch,
                    "wait_range: EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            // On-demand loading: request specific segment ONCE
            if !on_demand_requested && !segments.is_range_loaded(&range) {
                if segments.had_midstream_switch {
                    // After variant switch, metadata offsets don't match storage.
                    // Just wake the sequential downloader — it will fill the gap.
                    drop(segments);
                    self.shared.reader_advanced.notify_one();
                    on_demand_requested = true;
                    segments = self.shared.segments.lock();
                } else {
                    let current_variant = segments.last().map_or(0, |e| e.meta.variant);

                    drop(segments);
                    if let Some(segment_index) =
                        self.find_segment_for_offset(current_variant, range.start)
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
        let entry = {
            let segments = self.shared.segments.lock();
            segments.find_at_offset(offset).cloned()
        };

        let Some(entry) = entry else {
            return Ok(0);
        };

        // Variant fence: auto-detect on first read, block cross-variant reads.
        if self.variant_fence.is_none() {
            self.variant_fence = Some(entry.meta.variant);
        }
        if let Some(fence) = self.variant_fence
            && entry.meta.variant != fence
        {
            return Ok(0);
        }

        let bytes = self
            .read_from_entry(&entry, offset, buf)
            .map_err(StreamError::Source)?;

        if bytes > 0 {
            let new_pos = offset.saturating_add(bytes as u64);
            self.shared.reader_offset.store(new_pos, Ordering::Release);
            self.shared.reader_advanced.notify_one();

            let total = self.shared.segments.lock().total_bytes();
            self.emit_event(HlsEvent::PlaybackProgress {
                position: new_pos,
                total: Some(total),
            });
        }

        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        let segments = self.shared.segments.lock();
        let expected = segments.expected_total_length;
        drop(segments);
        if expected > 0 { Some(expected) } else { None }
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let segments = self.shared.segments.lock();
        let last = segments.last()?;
        Some(
            MediaInfo::new(last.codec, last.meta.container)
                .with_variant_index(last.meta.variant as u32),
        )
    }

    fn current_segment_range(&self) -> Option<Range<u64>> {
        let segments = self.shared.segments.lock();
        segments
            .last()
            .map(|entry| entry.byte_offset..entry.end_offset())
    }

    fn format_change_segment_range(&self) -> Option<Range<u64>> {
        let segments = self.shared.segments.lock();
        let last = segments.last()?;
        // Find the first segment of the current variant.
        // This is where init data (ftyp/moov) lives after an ABR switch.
        segments
            .first_segment_of_variant(last.meta.variant)
            .map(|entry| entry.byte_offset..entry.end_offset())
    }

    fn clear_variant_fence(&mut self) {
        self.variant_fence = None;
    }
}

/// Build an `HlsDownloader` + `HlsSource` pair from config.
pub fn build_pair(
    fetch: Arc<DefaultFetchManager>,
    variants: Vec<VariantStream>,
    config: &crate::config::HlsConfig,
    coverage_index: Option<Arc<kithara_assets::CoverageIndex<kithara_storage::MmapResource>>>,
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
    let shared = Arc::new(SharedSegments::new(cancel));

    let downloader = HlsDownloader {
        io: HlsIo::new(Arc::clone(&fetch)),
        fetch: Arc::clone(&fetch),
        variants,
        current_segment_index: 0,
        sent_init_for_variant: HashSet::new(),
        abr,
        byte_offset: 0,
        shared: Arc::clone(&shared),
        events_tx: config.events_tx.clone(),
        look_ahead_bytes: config.look_ahead_bytes,
        variant_lengths: HashMap::new(),
        had_midstream_switch: false,
        prefetch_count: config.download_batch_size.max(1),
        coverage_index,
    };

    let source = HlsSource {
        fetch,
        shared,
        events_tx: config.events_tx.clone(),
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
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::DecryptContext;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_stream::ContainerFormat;

    use super::*;
    use crate::fetch::{FetchManager, SegmentType};

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
        HlsSource {
            fetch,
            shared,
            events_tx: None,
            variant_fence: None,
            _backend: None,
        }
    }

    fn make_entry(
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
        len: u64,
    ) -> SegmentEntry {
        SegmentEntry {
            byte_offset,
            codec: Some(AudioCodec::AacLc),
            init_len: 0,
            init_url: None,
            meta: SegmentMeta {
                variant,
                segment_type: SegmentType::Media(segment_index),
                sequence: 0,
                url: Url::parse("https://example.com/seg").unwrap(),
                duration: Some(Duration::from_secs(4)),
                key: None,
                len,
                container: Some(ContainerFormat::Fmp4),
            },
        }
    }

    #[test]
    fn test_fence_at_removes_stale_entries() {
        let mut index = SegmentIndex::new();

        // V0 entries: 0..100, 100..200, 200..300
        index.push(make_entry(0, 0, 0, 100));
        index.push(make_entry(0, 1, 100, 100));
        index.push(make_entry(0, 2, 200, 100));

        // V3 entries: 300..400
        index.push(make_entry(3, 0, 300, 100));

        assert_eq!(index.entries.len(), 4);
        assert!(index.is_segment_loaded(0, 2));

        // Fence at offset 200, keep V3.
        // V0 entries at offset >= 200 should be removed (entry at 200..300).
        // V0 entries before offset 200 should be kept (entries at 0..100, 100..200).
        // V3 entries should be kept regardless.
        index.fence_at(200, 3);

        assert_eq!(index.entries.len(), 3);

        // V0 entries before fence are kept
        assert!(index.is_segment_loaded(0, 0));
        assert!(index.is_segment_loaded(0, 1));

        // V0 entry at/past fence is removed
        assert!(!index.is_segment_loaded(0, 2));

        // V3 entry is kept
        assert!(index.is_segment_loaded(3, 0));

        // loaded_ranges rebuilt correctly
        assert!(index.is_range_loaded(&(0..200)));
        assert!(!index.is_range_loaded(&(200..300)));
        assert!(index.is_range_loaded(&(300..400)));
    }

    #[test]
    fn test_find_at_offset_after_fence() {
        let mut index = SegmentIndex::new();

        // V0: 0..100, 100..200
        index.push(make_entry(0, 0, 0, 100));
        index.push(make_entry(0, 1, 100, 100));

        // V3: 200..300
        index.push(make_entry(3, 0, 200, 100));

        // Before fence, V0 entry at 100 is findable
        assert!(index.find_at_offset(150).is_some());
        assert_eq!(index.find_at_offset(150).unwrap().meta.variant, 0);

        // Fence at 100, keep V3
        index.fence_at(100, 3);

        // V0 entry at 100..200 removed → offset 150 not found
        assert!(index.find_at_offset(150).is_none());

        // V0 entry at 0..100 still there
        assert!(index.find_at_offset(50).is_some());
        assert_eq!(index.find_at_offset(50).unwrap().meta.variant, 0);

        // V3 entry at 200..300 still there
        assert!(index.find_at_offset(250).is_some());
        assert_eq!(index.find_at_offset(250).unwrap().meta.variant, 3);
    }

    #[test]
    fn test_wait_range_blocks_after_fence() {
        let mut index = SegmentIndex::new();

        // V0: 0..100, 100..200
        index.push(make_entry(0, 0, 0, 100));
        index.push(make_entry(0, 1, 100, 100));

        // V3: 200..300
        index.push(make_entry(3, 0, 200, 100));

        // Range 100..200 is loaded (V0 entry)
        assert!(index.is_range_loaded(&(100..200)));

        // Fence at 100, keep V3
        index.fence_at(100, 3);

        // Range 100..200 is no longer loaded (V0 entry removed)
        assert!(!index.is_range_loaded(&(100..200)));

        // Range 0..100 still loaded (V0 entry before fence)
        assert!(index.is_range_loaded(&(0..100)));

        // Range 200..300 still loaded (V3 entry)
        assert!(index.is_range_loaded(&(200..300)));
    }

    #[test]
    fn test_cumulative_offset_after_switch() {
        let mut index = SegmentIndex::new();

        // Simulate V0 segments 0..13 occupying 0..700
        for i in 0..14 {
            index.push(make_entry(0, i, i as u64 * 50, 50));
        }

        assert_eq!(index.total_bytes(), 700);
        assert!(index.is_range_loaded(&(0..700)));

        // Simulate variant switch: fence at 700, keep V3
        index.fence_at(700, 3);

        // V3 seg 14 placed at cumulative offset 700 (not metadata offset)
        index.push(make_entry(3, 14, 700, 200));

        // V3 seg 15 placed contiguously at 900
        index.push(make_entry(3, 15, 900, 200));

        // Verify contiguous layout with no gaps
        assert!(index.is_range_loaded(&(0..700)));
        assert!(index.is_range_loaded(&(700..900)));
        assert!(index.is_range_loaded(&(900..1100)));
        assert!(index.is_range_loaded(&(0..1100)));

        // No gap between V0 and V3
        assert!(index.find_at_offset(700).is_some());
        assert_eq!(index.find_at_offset(700).unwrap().meta.variant, 3);
        assert_eq!(index.find_at_offset(700).unwrap().segment_index(), 14);
    }

    #[test]
    fn test_last_entry_tracks_most_recent_push() {
        let mut index = SegmentIndex::new();

        assert!(index.last().is_none());

        index.push(make_entry(0, 0, 0, 100));
        assert_eq!(index.last().unwrap().segment_index(), 0);
        assert_eq!(index.last().unwrap().meta.variant, 0);

        index.push(make_entry(0, 1, 100, 100));
        assert_eq!(index.last().unwrap().segment_index(), 1);

        // After variant switch
        index.push(make_entry(3, 14, 200, 100));
        assert_eq!(index.last().unwrap().meta.variant, 3);
        assert_eq!(index.last().unwrap().segment_index(), 14);
    }

    #[test]
    fn test_had_midstream_switch_flag() {
        let mut index = SegmentIndex::new();
        assert!(!index.had_midstream_switch);

        index.had_midstream_switch = true;
        assert!(index.had_midstream_switch);
    }

    #[test]
    fn test_total_bytes_with_hashmap() {
        let mut index = SegmentIndex::new();
        assert_eq!(index.total_bytes(), 0);

        // Entries from different variants at different offsets
        index.push(make_entry(0, 0, 0, 100));
        assert_eq!(index.total_bytes(), 100);

        index.push(make_entry(0, 1, 100, 200));
        assert_eq!(index.total_bytes(), 300);

        // V3 entry at higher offset
        index.push(make_entry(3, 5, 500, 100));
        assert_eq!(index.total_bytes(), 600);
    }

    // --- wait_range cancellation tests ---

    #[tokio::test]
    async fn test_wait_range_cancel_unblocks() {
        // Cancel token should unblock wait_range within 100ms.
        let cancel = CancellationToken::new();
        let shared = Arc::new(SharedSegments::new(cancel.clone()));
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
        let shared = Arc::new(SharedSegments::new(cancel.clone()));
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
        let shared = Arc::new(SharedSegments::new(cancel.clone()));
        let shared2 = Arc::clone(&shared);
        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let handle = tokio::task::spawn_blocking(move || source.wait_range(0..100));

        // Push a segment covering 0..100
        tokio::time::sleep(Duration::from_millis(20)).await;
        {
            let mut segments = shared2.segments.lock();
            segments.push(make_entry(0, 0, 0, 100));
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
        // Downloader stopped + eof → wait_range at past-end offset returns Eof.
        let cancel = CancellationToken::new();
        let shared = Arc::new(SharedSegments::new(cancel.clone()));
        let shared2 = Arc::clone(&shared);

        // Push one segment
        {
            let mut segments = shared2.segments.lock();
            segments.push(make_entry(0, 0, 0, 100));
        }
        // Mark eof + stopped
        shared2.eof.store(true, Ordering::Release);
        shared2.stopped.store(true, Ordering::Release);

        let mut source = make_test_source(Arc::clone(&shared), cancel.clone());

        let result = source.wait_range(100..200);
        assert!(matches!(result, Ok(WaitOutcome::Eof)));
    }
}
