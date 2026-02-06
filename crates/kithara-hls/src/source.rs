//! HLS source and downloader.
//!
//! `HlsDownloader` implements `Downloader` — fetches segments in background.
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! They share state via `SharedSegments`.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crossbeam_queue::SegQueue;
use kithara_abr::{
    AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource, Variant,
};
use kithara_assets::{AssetStore, Assets, ResourceKey};
use kithara_storage::{ResourceExt, ResourceStatus, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, Downloader, MediaInfo, Source, StreamError, StreamResult,
};
use parking_lot::{Condvar, Mutex};
use rangemap::RangeSet;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    events::HlsEvent,
    fetch::{DefaultFetchManager, Loader},
};

/// Metadata for an HLS variant.
#[derive(Debug, Clone)]
pub struct VariantMetadata {
    /// Variant index in master playlist.
    pub index: usize,
    /// Audio codec.
    pub codec: Option<AudioCodec>,
    /// Container format (fMP4, TS, etc.).
    #[allow(dead_code)]
    pub container: Option<ContainerFormat>,
    /// Advertised bitrate in bits per second.
    pub bitrate: Option<u64>,
}

/// Metadata for a single segment in a variant's theoretical stream.
#[derive(Debug, Clone)]
struct SegmentMetadata {
    /// Segment index in playlist.
    #[allow(dead_code)]
    index: usize,
    /// Byte offset in theoretical stream (cumulative from segment 0).
    byte_offset: u64,
    /// Total size (init + media) in bytes.
    size: u64,
}

/// Request to load a specific segment (on-demand or sequential).
#[derive(Debug, Clone)]
struct SegmentRequest {
    variant: usize,
    segment_index: usize,
}

/// Entry tracking a loaded segment in the virtual stream.
#[derive(Debug, Clone)]
struct SegmentEntry {
    segment_index: usize,
    segment_url: Url,
    init_url: Option<Url>,
    byte_offset: u64,
    init_len: u64,
    media_len: u64,
    #[allow(dead_code)]
    variant: usize,
    container: Option<ContainerFormat>,
    codec: Option<AudioCodec>,
}

impl SegmentEntry {
    fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }

    fn end_offset(&self) -> u64 {
        self.byte_offset + self.total_len()
    }

    fn contains(&self, offset: u64) -> bool {
        offset >= self.byte_offset && offset < self.end_offset()
    }
}

/// Index of loaded segments.
#[derive(Debug)]
struct SegmentIndex {
    entries: HashMap<(usize, usize), SegmentEntry>,
    /// Byte ranges that have been loaded (reflects actual data on disk).
    loaded_ranges: RangeSet<u64>,
    /// Expected total length of current variant's theoretical stream.
    /// Used for seek bounds checking by Symphonia.
    expected_total_length: u64,
    /// Key of the most recently pushed entry (for last(), media_info, segment_range).
    last_entry_key: Option<(usize, usize)>,
    /// True after a mid-stream variant switch. On-demand loading should
    /// just wake the sequential downloader instead of using metadata lookups.
    had_midstream_switch: bool,
}

impl Default for SegmentIndex {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            loaded_ranges: RangeSet::new(),
            expected_total_length: 0,
            last_entry_key: None,
            had_midstream_switch: false,
        }
    }
}

impl SegmentIndex {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, entry: SegmentEntry) {
        let end = entry.end_offset();
        let range = entry.byte_offset..end;
        self.loaded_ranges.insert(range);
        let key = (entry.variant, entry.segment_index);
        if end > self.expected_total_length {
            self.expected_total_length = end;
        }
        debug!(
            variant = entry.variant,
            segment_index = entry.segment_index,
            byte_offset = entry.byte_offset,
            end,
            "segment_index::push"
        );
        self.last_entry_key = Some(key);
        self.entries.insert(key, entry);
    }

    fn is_segment_loaded(&self, variant: usize, segment_index: usize) -> bool {
        self.entries.contains_key(&(variant, segment_index))
    }

    fn is_range_loaded(&self, range: &Range<u64>) -> bool {
        !self.loaded_ranges.gaps(range).any(|_| true)
    }

    fn last(&self) -> Option<&SegmentEntry> {
        self.last_entry_key.and_then(|key| self.entries.get(&key))
    }

    fn find_at_offset(&self, offset: u64) -> Option<&SegmentEntry> {
        self.entries.values().find(|e| e.contains(offset))
    }

    /// Find the first segment of the given variant (by lowest byte_offset).
    ///
    /// Used to find the start of a new variant after ABR switch — this is where
    /// init data (ftyp/moov) lives for the new variant.
    fn first_segment_of_variant(&self, variant: usize) -> Option<&SegmentEntry> {
        self.entries
            .values()
            .filter(|e| e.variant == variant)
            .min_by_key(|e| e.byte_offset)
    }

    fn total_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(|e| e.end_offset())
            .max()
            .unwrap_or(0)
    }

    /// Remove all entries from other variants at or past the fence offset.
    /// Rebuilds `loaded_ranges` from remaining entries.
    fn fence_at(&mut self, offset: u64, keep_variant: usize) {
        self.entries
            .retain(|_, e| e.byte_offset < offset || e.variant == keep_variant);

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

/// Shared state between HlsDownloader and HlsSource.
struct SharedSegments {
    segments: Mutex<SegmentIndex>,
    /// Downloader → Source: new segment available (for sync blocking in Source).
    condvar: Condvar,
    eof: AtomicBool,
    /// Current reader byte offset (updated by Source on every read_at).
    reader_offset: AtomicU64,
    /// Source → Downloader: reader advanced, may resume downloading.
    reader_advanced: Notify,
    /// Segment load requests (on-demand from seek or sequential).
    segment_requests: SegQueue<SegmentRequest>,
    /// Per-variant segment metadata for byte_offset → segment_index mapping.
    variant_metadata: Mutex<HashMap<usize, Vec<SegmentMetadata>>>,
}

impl SharedSegments {
    fn new() -> Self {
        Self {
            segments: Mutex::new(SegmentIndex::new()),
            condvar: Condvar::new(),
            eof: AtomicBool::new(false),
            reader_offset: AtomicU64::new(0),
            reader_advanced: Notify::new(),
            segment_requests: SegQueue::new(),
            variant_metadata: Mutex::new(HashMap::new()),
        }
    }
}

/// HLS downloader: fetches segments and maintains ABR state.
pub struct HlsDownloader {
    fetch: Arc<DefaultFetchManager>,
    variant_metadata: Vec<VariantMetadata>,
    current_segment_index: usize,
    sent_init_for_variant: HashSet<usize>,
    abr: AbrController<ThroughputEstimator>,
    byte_offset: u64,
    shared: Arc<SharedSegments>,
    events_tx: Option<broadcast::Sender<HlsEvent>>,
    cancel: CancellationToken,
    look_ahead_bytes: u64,
    /// Cache of expected_total_length per variant (variant_index → total_bytes).
    variant_lengths: HashMap<usize, u64>,
    /// True after a mid-stream variant switch. Subsequent segments use cumulative offsets.
    had_midstream_switch: bool,
}

impl HlsDownloader {
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Calculate expected total length and per-segment metadata via HEAD requests.
    ///
    /// Returns `(total_length, metadata, init_url, init_len, media_urls)`.
    async fn calculate_variant_metadata(
        fetch: &Arc<DefaultFetchManager>,
        variant: usize,
    ) -> Result<(u64, Vec<SegmentMetadata>, Option<Url>, u64, Vec<Url>), HlsError> {
        let (init_url, media_urls) = fetch.get_segment_urls(variant).await?;

        let init_len = if let Some(ref url) = init_url {
            fetch.get_content_length(url).await.unwrap_or(0)
        } else {
            0
        };

        let mut total = 0u64;
        let mut metadata = Vec::new();
        let mut byte_offset = 0u64;

        for (index, segment_url) in media_urls.iter().enumerate() {
            if let Ok(media_len) = fetch.get_content_length(segment_url).await {
                // Init segment included only once (first segment of variant)
                let size = if index == 0 {
                    init_len + media_len
                } else {
                    media_len
                };
                metadata.push(SegmentMetadata {
                    index,
                    byte_offset,
                    size,
                });
                byte_offset += size;
                total += size;
            }
        }

        debug!(
            variant,
            total,
            num_segments = metadata.len(),
            "calculated variant metadata"
        );
        Ok((total, metadata, init_url, init_len, media_urls))
    }

    /// Pre-populate segment index with segments already committed on disk.
    ///
    /// Scans the asset store for committed segment resources and creates entries
    /// so that `loaded_ranges` reflects actual disk state. Uses metadata offsets
    /// for consistency with `expected_total_length` and sidx-based seeking.
    ///
    /// Init data is included only for the first segment (init-once layout).
    /// Stops at the first uncached segment to maintain contiguous entries.
    ///
    /// Returns `(count, final_byte_offset)` for caller to update downloader state.
    #[allow(clippy::too_many_arguments)]
    fn populate_cached_segments(
        shared: &SharedSegments,
        fetch: &DefaultFetchManager,
        variant: usize,
        metadata: &[SegmentMetadata],
        init_url: &Option<Url>,
        init_len: u64,
        media_urls: &[Url],
        variant_meta: &VariantMetadata,
    ) -> (usize, u64) {
        let assets = fetch.assets();
        let container = if init_url.is_some() {
            Some(ContainerFormat::Fmp4)
        } else {
            Some(ContainerFormat::MpegTs)
        };

        // If init is required, verify it's cached on disk
        if let Some(url) = init_url {
            let init_key = ResourceKey::from_url(url);
            let init_cached = assets
                .open_resource(&init_key)
                .map(|r| matches!(r.status(), ResourceStatus::Committed { .. }))
                .unwrap_or(false);
            if !init_cached {
                return (0, 0);
            }
        }

        let mut segments = shared.segments.lock();
        let mut count = 0usize;
        let mut final_offset = 0u64;

        for (index, (meta, segment_url)) in metadata.iter().zip(media_urls.iter()).enumerate() {
            let key = ResourceKey::from_url(segment_url);
            let resource = match assets.open_resource(&key) {
                Ok(r) => r,
                Err(_) => break, // Stop on first uncached segment
            };

            if let ResourceStatus::Committed { final_len } = resource.status() {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                // Init only for the first segment (init-once layout matches metadata)
                let actual_init_len = if count == 0 { init_len } else { 0 };

                let entry = SegmentEntry {
                    segment_index: index,
                    segment_url: segment_url.clone(),
                    init_url: init_url.clone(),
                    byte_offset: meta.byte_offset,
                    init_len: actual_init_len,
                    media_len,
                    variant,
                    container,
                    codec: variant_meta.codec,
                };

                final_offset = entry.end_offset();
                segments.push(entry);
                count += 1;
            } else {
                break; // Not committed, stop
            }
        }

        drop(segments);
        if count > 0 {
            debug!(
                variant,
                count, final_offset, "pre-populated cached segments from disk"
            );
            shared.condvar.notify_all();
        }

        (count, final_offset)
    }

    /// Fetch a specific segment by variant and index.
    async fn fetch_segment(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Result<(), HlsError> {
        if self.cancel.is_cancelled() {
            debug!("cancelled, skipping fetch");
            return Ok(());
        }

        let num_segments = self.fetch.num_segments(variant).await?;
        if segment_index >= num_segments {
            debug!(variant, segment_index, "segment index out of range");
            return Ok(());
        }

        // Check if already loaded (e.g., from cache pre-population)
        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(variant, segment_index)
        {
            debug!(variant, segment_index, "segment already loaded, skipping");
            return Ok(());
        }

        let is_variant_switch = !self.sent_init_for_variant.contains(&variant);
        let is_midstream_switch = is_variant_switch && segment_index > 0;

        // Set midstream switch flag EARLY (before download) so that the reader's
        // wait_range won't queue stale on-demand requests for the old variant.
        if is_midstream_switch {
            self.had_midstream_switch = true;
            self.shared.segments.lock().had_midstream_switch = true;
            // Discard any pending on-demand requests from before the switch.
            while self.shared.segment_requests.pop().is_some() {}
        }

        // After a midstream switch, skip segments from the old variant.
        // These are stale on-demand requests queued before the switch was detected.
        if self.had_midstream_switch && !is_midstream_switch {
            let current_variant = self.abr.get_current_variant_index();
            if variant != current_variant {
                debug!(
                    variant,
                    segment_index,
                    current_variant,
                    "skipping stale segment from pre-switch variant"
                );
                return Ok(());
            }
        }

        // Calculate and cache metadata for this variant if needed
        let current_length = self.shared.segments.lock().expected_total_length;
        if is_variant_switch || current_length == 0 {
            let needs_calculation = {
                let metadata_map = self.shared.variant_metadata.lock();
                !metadata_map.contains_key(&variant)
            };

            if needs_calculation {
                match Self::calculate_variant_metadata(&self.fetch, variant).await {
                    Ok((total, metadata, init_url, init_len, media_urls)) => {
                        debug!(
                            variant,
                            total,
                            num_segments = metadata.len(),
                            "calculated and caching variant metadata"
                        );

                        // Pre-populate cached segments from disk.
                        // Skip for mid-stream variant switches: metadata offsets start
                        // from 0 but the reader is at a global stream position, so
                        // entries would be placed at wrong offsets and never found.
                        let (cached_count, cached_end_offset) = if !is_variant_switch {
                            let vmeta = &self.variant_metadata[variant];
                            Self::populate_cached_segments(
                                &self.shared,
                                &self.fetch,
                                variant,
                                &metadata,
                                &init_url,
                                init_len,
                                &media_urls,
                                vmeta,
                            )
                        } else {
                            (0, 0)
                        };

                        // Advance downloader state past pre-populated segments
                        if cached_count > 0 {
                            if cached_end_offset > self.byte_offset {
                                self.byte_offset = cached_end_offset;
                            }
                            if cached_count > self.current_segment_index {
                                self.current_segment_index = cached_count;
                            }
                            self.sent_init_for_variant.insert(variant);
                        }

                        // After a mid-stream variant switch, the metadata total
                        // reflects the full variant from segment 0, but we started
                        // mid-stream. Adjust: actual_total = switch_byte_offset +
                        // (variant_total - switch_metadata_offset).
                        let effective_total = if is_midstream_switch {
                            let switch_byte = self.byte_offset;
                            let switch_meta = metadata
                                .get(segment_index)
                                .map(|s| s.byte_offset)
                                .unwrap_or(0);
                            let result = total
                                .saturating_sub(switch_meta)
                                .saturating_add(switch_byte);
                            debug!(
                                variant,
                                segment_index,
                                metadata_total = total,
                                switch_byte,
                                switch_meta,
                                effective_total = result,
                                "effective_total: midstream switch adjustment"
                            );
                            result
                        } else {
                            let result = total.max(cached_end_offset);
                            debug!(
                                variant,
                                metadata_total = total,
                                cached_end_offset,
                                effective_total = result,
                                "effective_total: normal (no switch)"
                            );
                            result
                        };

                        self.shared
                            .variant_metadata
                            .lock()
                            .insert(variant, metadata);
                        self.variant_lengths.insert(variant, effective_total);

                        if effective_total > 0 {
                            self.shared.segments.lock().expected_total_length = effective_total;
                        }
                    }
                    Err(e) => {
                        debug!(?e, variant, "failed to calculate variant metadata");
                    }
                }
            } else if let Some(&cached_length) = self.variant_lengths.get(&variant) {
                debug!(
                    variant,
                    total = cached_length,
                    "using cached variant length"
                );
                if cached_length > 0 {
                    self.shared.segments.lock().expected_total_length = cached_length;
                }
            }
        }

        // Re-check after metadata calculation (populate_cached_segments may have loaded it)
        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(variant, segment_index)
        {
            debug!(
                variant,
                segment_index, "segment loaded from cache after metadata calc"
            );
            return Ok(());
        }

        self.emit_event(HlsEvent::SegmentStart {
            variant,
            segment_index,
            byte_offset: self.byte_offset,
        });

        let fetch_start = Instant::now();
        let meta = self.fetch.load_segment(variant, segment_index).await?;
        let fetch_duration = fetch_start.elapsed();

        self.record_throughput(meta.len, fetch_duration, meta.duration);

        self.emit_event(HlsEvent::SegmentComplete {
            variant,
            segment_index,
            bytes_transferred: meta.len,
            duration: fetch_duration,
        });

        // Get init segment info
        let (init_url, init_len) = match self.fetch.load_segment(variant, usize::MAX).await {
            Ok(init_meta) => (Some(init_meta.url), init_meta.len),
            Err(_) => (None, 0),
        };

        // Mark variant as initialized regardless of whether init segment exists.
        // For non-init playlists (MPEG-TS), the init load fails but we still
        // need to track that this variant has been seen to prevent subsequent
        // segments from being treated as variant switches.
        if is_variant_switch {
            self.sent_init_for_variant.insert(variant);
        }

        // Determine byte_offset and init_len for this entry.
        // Init-once layout: init included only for the first segment of each variant.
        // After a mid-stream variant switch, metadata offsets (which start from 0 for
        // the new variant) don't match the cumulative stream position, so we use
        // cumulative byte_offset for all subsequent segments.
        let (byte_offset, actual_init_len) = if is_midstream_switch {
            // First segment after mid-stream switch: cumulative offset + init
            (self.byte_offset, init_len)
        } else if self.had_midstream_switch {
            // Subsequent segments after switch: cumulative offset, no init
            (self.byte_offset, 0)
        } else {
            // Normal: metadata offsets
            let offset = {
                let metadata_map = self.shared.variant_metadata.lock();
                if let Some(metadata) = metadata_map.get(&variant)
                    && let Some(seg_meta) = metadata.get(segment_index)
                {
                    seg_meta.byte_offset
                } else {
                    self.byte_offset
                }
            };
            let init = if is_variant_switch { init_len } else { 0 };
            (offset, init)
        };

        let variant_meta = &self.variant_metadata[variant];

        let entry = SegmentEntry {
            segment_index,
            segment_url: meta.url.clone(),
            init_url,
            byte_offset,
            init_len: actual_init_len,
            media_len: meta.len,
            variant,
            container: meta.container,
            codec: variant_meta.codec,
        };

        let end = byte_offset + actual_init_len + meta.len;
        if end > self.byte_offset {
            self.byte_offset = end;
        }

        self.emit_event(HlsEvent::DownloadProgress {
            offset: self.byte_offset,
            total: None,
        });

        {
            let mut segments = self.shared.segments.lock();
            if is_variant_switch {
                segments.fence_at(byte_offset, variant);
            }
            segments.push(entry);
        }
        self.shared.condvar.notify_all();

        Ok(())
    }

    fn make_abr_decision(&mut self) -> kithara_abr::AbrDecision {
        let now = Instant::now();
        let decision = self.abr.decide(now);

        if decision.changed {
            debug!(
                from = self.abr.get_current_variant_index(),
                to = decision.target_variant_index,
                reason = ?decision.reason,
                "ABR variant switch"
            );
            self.abr.apply(&decision, now);
        }

        decision
    }

    fn record_throughput(
        &mut self,
        bytes: u64,
        duration: Duration,
        content_duration: Option<Duration>,
    ) {
        if duration.as_millis() < 10 {
            return;
        }

        let sample = ThroughputSample {
            bytes,
            duration,
            at: Instant::now(),
            source: ThroughputSampleSource::Network,
            content_duration,
        };

        self.abr.push_throughput_sample(sample);

        let bytes_per_second = if duration.as_secs_f64() > 0.0 {
            bytes as f64 / duration.as_secs_f64()
        } else {
            0.0
        };
        self.emit_event(HlsEvent::ThroughputSample { bytes_per_second });
    }
}

impl Downloader for HlsDownloader {
    async fn step(&mut self) -> bool {
        // Backpressure: wait if too far ahead of reader.
        // On-demand requests (from wait_range during seek/atom scan) bypass
        // backpressure to prevent deadlock: reader blocked in condvar.wait()
        // can't advance reader_pos, so the gap never shrinks.
        loop {
            if !self.shared.segment_requests.is_empty() {
                break;
            }

            let advanced = self.shared.reader_advanced.notified();
            tokio::pin!(advanced);

            let reader_pos = self.shared.reader_offset.load(Ordering::Acquire);
            let downloaded = self.shared.segments.lock().total_bytes();

            if downloaded.saturating_sub(reader_pos) <= self.look_ahead_bytes {
                break;
            }

            debug!(
                downloaded,
                reader_pos,
                gap = downloaded - reader_pos,
                "downloader waiting for reader to catch up"
            );
            advanced.await;
        }

        // Unified queue: check for on-demand requests first, otherwise add sequential
        if self.shared.segment_requests.is_empty() {
            // Make ABR decision for next sequential segment
            let old_variant = self.abr.get_current_variant_index();
            let decision = self.make_abr_decision();
            let next_variant = self.abr.get_current_variant_index();

            let num_segments = self
                .fetch
                .num_segments(next_variant)
                .await
                .ok()
                .unwrap_or(0);

            if self.current_segment_index >= num_segments {
                debug!("reached end of playlist, stopping downloader");
                self.emit_event(HlsEvent::EndOfStream);
                self.shared.eof.store(true, Ordering::Release);
                self.shared.condvar.notify_all();
                return false;
            }

            if decision.changed {
                self.emit_event(HlsEvent::VariantApplied {
                    from_variant: old_variant,
                    to_variant: next_variant,
                    reason: decision.reason,
                });
            }

            self.shared.segment_requests.push(SegmentRequest {
                variant: next_variant,
                segment_index: self.current_segment_index,
            });
        }

        // Process from queue (on-demand or sequential)
        if let Some(req) = self.shared.segment_requests.pop() {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                "processing segment request from queue"
            );

            match self.fetch_segment(req.variant, req.segment_index).await {
                Ok(()) => {
                    // Always wake reader — fetch_segment may return early
                    // (e.g. "already loaded from cache") without notifying condvar.
                    self.shared.condvar.notify_all();

                    // Update current_segment_index only for sequential loads
                    if req.segment_index == self.current_segment_index {
                        self.current_segment_index += 1;
                    }
                    true
                }
                Err(e) => {
                    // Wake reader even on error to avoid deadlock
                    self.shared.condvar.notify_all();

                    debug!(?e, "segment fetch error");
                    self.emit_event(HlsEvent::DownloadError {
                        error: e.to_string(),
                    });
                    false
                }
            }
        } else {
            true
        }
    }
}

/// HLS source: provides random-access reading from loaded segments.
pub struct HlsSource {
    fetch: Arc<DefaultFetchManager>,
    shared: Arc<SharedSegments>,
    events_tx: Option<broadcast::Sender<HlsEvent>>,
}

impl HlsSource {
    /// Get asset store.
    pub fn assets(&self) -> AssetStore {
        self.fetch.assets().clone()
    }

    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Find segment index for a given byte offset using variant metadata.
    /// Returns segment_index for the segment containing the offset.
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
        let assets = self.assets();

        if local_offset < entry.init_len {
            let Some(ref init_url) = entry.init_url else {
                return Ok(0);
            };

            let key = ResourceKey::from_url(init_url);
            let resource = assets.open_resource(&key)?;

            let read_end = (local_offset + buf.len() as u64).min(entry.init_len);
            resource.wait_range(local_offset..read_end)?;

            let available = (entry.init_len - local_offset) as usize;
            let to_read = buf.len().min(available);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read])?;

            if bytes_from_init < buf.len() && entry.media_len > 0 {
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
        let assets = self.assets();
        let key = ResourceKey::from_url(&entry.segment_url);
        let resource = assets.open_resource(&key)?;

        let read_end = (media_offset + buf.len() as u64).min(entry.media_len);
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
                    let current_variant = segments.last().map(|e| e.variant).unwrap_or(0);

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
                            variant: current_variant,
                            segment_index,
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

            // Block until downloader pushes more segments
            self.shared.condvar.wait(&mut segments);
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
        Some(MediaInfo::new(last.codec, last.container).with_variant_index(last.variant as u32))
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
            .first_segment_of_variant(last.variant)
            .map(|entry| entry.byte_offset..entry.end_offset())
    }
}

/// Build an HlsDownloader + HlsSource pair from config.
pub fn build_pair(
    fetch: Arc<DefaultFetchManager>,
    variant_metadata: Vec<VariantMetadata>,
    config: &crate::config::HlsConfig,
) -> (HlsDownloader, HlsSource) {
    let cancel = config.cancel.clone().unwrap_or_default();

    let variants: Vec<Variant> = variant_metadata
        .iter()
        .map(|v| Variant {
            variant_index: v.index,
            bandwidth_bps: v.bitrate.unwrap_or(0),
        })
        .collect();

    let mut abr_opts = config.abr.clone();
    abr_opts.variants = variants;

    let abr = AbrController::new(abr_opts);
    let shared = Arc::new(SharedSegments::new());

    let downloader = HlsDownloader {
        fetch: Arc::clone(&fetch),
        variant_metadata,
        current_segment_index: 0,
        sent_init_for_variant: HashSet::new(),
        abr,
        byte_offset: 0,
        shared: Arc::clone(&shared),
        events_tx: config.events_tx.clone(),
        cancel,
        look_ahead_bytes: config.look_ahead_bytes,
        variant_lengths: HashMap::new(),
        had_midstream_switch: false,
    };

    let source = HlsSource {
        fetch,
        shared,
        events_tx: config.events_tx.clone(),
    };

    (downloader, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
        len: u64,
    ) -> SegmentEntry {
        SegmentEntry {
            segment_index,
            segment_url: Url::parse("https://example.com/seg").unwrap(),
            init_url: None,
            byte_offset,
            init_len: 0,
            media_len: len,
            variant,
            container: Some(ContainerFormat::Fmp4),
            codec: Some(AudioCodec::AacLc),
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
        assert_eq!(index.find_at_offset(150).unwrap().variant, 0);

        // Fence at 100, keep V3
        index.fence_at(100, 3);

        // V0 entry at 100..200 removed → offset 150 not found
        assert!(index.find_at_offset(150).is_none());

        // V0 entry at 0..100 still there
        assert!(index.find_at_offset(50).is_some());
        assert_eq!(index.find_at_offset(50).unwrap().variant, 0);

        // V3 entry at 200..300 still there
        assert!(index.find_at_offset(250).is_some());
        assert_eq!(index.find_at_offset(250).unwrap().variant, 3);
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
        assert_eq!(index.find_at_offset(700).unwrap().variant, 3);
        assert_eq!(index.find_at_offset(700).unwrap().segment_index, 14);
    }

    #[test]
    fn test_last_entry_tracks_most_recent_push() {
        let mut index = SegmentIndex::new();

        assert!(index.last().is_none());

        index.push(make_entry(0, 0, 0, 100));
        assert_eq!(index.last().unwrap().segment_index, 0);
        assert_eq!(index.last().unwrap().variant, 0);

        index.push(make_entry(0, 1, 100, 100));
        assert_eq!(index.last().unwrap().segment_index, 1);

        // After variant switch
        index.push(make_entry(3, 14, 200, 100));
        assert_eq!(index.last().unwrap().variant, 3);
        assert_eq!(index.last().unwrap().segment_index, 14);
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
}
