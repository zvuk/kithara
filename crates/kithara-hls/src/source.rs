//! HLS source and downloader.
//!
//! `HlsDownloader` implements `Downloader` — fetches segments in background.
//! `HlsSource` implements `Source` — provides random-access reading from loaded segments.
//! They share state via `SharedSegments`.

use std::{
    collections::HashSet,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use kithara_abr::{
    AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource, Variant,
};
use kithara_assets::{AssetStore, Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, Downloader, MediaInfo, Source, StreamError, StreamResult,
};
use parking_lot::Mutex;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    events::HlsEvent,
    fetch::{DefaultFetchManager, Loader, StreamingAssetResource},
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

/// Entry tracking a loaded segment in the virtual stream.
#[derive(Debug, Clone)]
struct SegmentEntry {
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
#[derive(Debug, Default)]
struct SegmentIndex {
    entries: Vec<SegmentEntry>,
}

impl SegmentIndex {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn push(&mut self, entry: SegmentEntry) {
        self.entries.push(entry);
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn last(&self) -> Option<&SegmentEntry> {
        self.entries.last()
    }

    fn find_at_offset(&self, offset: u64) -> Option<&SegmentEntry> {
        self.entries.iter().find(|e| e.contains(offset))
    }

    fn range_covered(&self, range: &Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }
        if self.entries.is_empty() {
            return false;
        }
        let mut current_pos = range.start;
        for entry in &self.entries {
            if entry.byte_offset > current_pos {
                return false;
            }
            if entry.end_offset() > current_pos {
                current_pos = entry.end_offset();
            }
            if current_pos >= range.end {
                return true;
            }
        }
        current_pos >= range.end
    }

    fn total_bytes(&self) -> u64 {
        self.entries.last().map(|e| e.end_offset()).unwrap_or(0)
    }
}

/// Shared state between HlsDownloader and HlsSource.
struct SharedSegments {
    segments: Mutex<SegmentIndex>,
    /// Downloader → Source: new segment available.
    notify: Notify,
    eof: AtomicBool,
    /// Current reader byte offset (updated by Source on every read_at).
    reader_offset: AtomicU64,
    /// Source → Downloader: reader advanced, may resume downloading.
    reader_advanced: Notify,
}

impl SharedSegments {
    fn new() -> Self {
        Self {
            segments: Mutex::new(SegmentIndex::new()),
            notify: Notify::new(),
            eof: AtomicBool::new(false),
            reader_offset: AtomicU64::new(0),
            reader_advanced: Notify::new(),
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
}

impl HlsDownloader {
    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Fetch next segment and add to shared index. Returns true if EOF.
    async fn fetch_next_segment(&mut self) -> Result<bool, HlsError> {
        if self.cancel.is_cancelled() {
            debug!("cancelled, returning EOF");
            return Ok(true);
        }

        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let current_variant = self.abr.get_current_variant_index();

        let is_variant_switch = !self.sent_init_for_variant.contains(&current_variant);

        let num_segments = self.fetch.num_segments(current_variant).await?;

        if self.current_segment_index >= num_segments {
            debug!("reached end of playlist");
            self.emit_event(HlsEvent::EndOfStream);
            return Ok(true);
        }

        self.emit_event(HlsEvent::SegmentStart {
            variant: current_variant,
            segment_index: self.current_segment_index,
            byte_offset: self.byte_offset,
        });

        let fetch_start = Instant::now();

        let meta = self
            .fetch
            .load_segment(current_variant, self.current_segment_index)
            .await?;

        let fetch_duration = fetch_start.elapsed();

        self.record_throughput(meta.len, fetch_duration, meta.duration);

        self.emit_event(HlsEvent::SegmentComplete {
            variant: current_variant,
            segment_index: self.current_segment_index,
            bytes_transferred: meta.len,
            duration: fetch_duration,
        });

        if decision.changed {
            self.emit_event(HlsEvent::VariantApplied {
                from_variant: old_variant,
                to_variant: current_variant,
                reason: decision.reason,
            });
        }

        // Get init segment info
        let (init_url, init_len) = match self.fetch.load_segment(current_variant, usize::MAX).await
        {
            Ok(init_meta) => {
                if is_variant_switch {
                    self.sent_init_for_variant.insert(current_variant);
                }
                (Some(init_meta.url), init_meta.len)
            }
            Err(_) => (None, 0),
        };

        let variant_meta = &self.variant_metadata[current_variant];
        let actual_init_len = if is_variant_switch { init_len } else { 0 };

        let entry = SegmentEntry {
            segment_url: meta.url.clone(),
            init_url,
            byte_offset: self.byte_offset,
            init_len: actual_init_len,
            media_len: meta.len,
            variant: current_variant,
            container: meta.container,
            codec: variant_meta.codec,
        };

        self.byte_offset += actual_init_len + meta.len;
        self.current_segment_index += 1;

        // Emit download progress
        self.emit_event(HlsEvent::DownloadProgress {
            offset: self.byte_offset,
            total: None,
        });

        // Push to shared index and notify waiting readers
        self.shared.segments.lock().push(entry);
        self.shared.notify.notify_waiters();

        Ok(false)
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

#[async_trait]
impl Downloader for HlsDownloader {
    async fn step(&mut self) -> bool {
        // Backpressure: wait if too far ahead of reader.
        loop {
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

        match self.fetch_next_segment().await {
            Ok(is_eof) => {
                if is_eof {
                    self.shared.eof.store(true, Ordering::Release);
                    self.shared.notify.notify_waiters();
                    return false;
                }
                true
            }
            Err(e) => {
                debug!(?e, "HlsDownloader fetch error");
                self.emit_event(HlsEvent::DownloadError {
                    error: e.to_string(),
                });
                self.shared.eof.store(true, Ordering::Release);
                self.shared.notify.notify_waiters();
                false
            }
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

    /// Read from a segment entry.
    async fn read_from_entry(
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
            let resource: StreamingAssetResource = assets.open_streaming_resource(&key).await?;

            let read_end = (local_offset + buf.len() as u64).min(entry.init_len);
            resource.wait_range(local_offset..read_end).await?;

            let available = (entry.init_len - local_offset) as usize;
            let to_read = buf.len().min(available);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read]).await?;

            if bytes_from_init < buf.len() && entry.media_len > 0 {
                let remaining = &mut buf[bytes_from_init..];
                let bytes_from_media = self.read_media_segment(entry, 0, remaining).await?;
                Ok(bytes_from_init + bytes_from_media)
            } else {
                Ok(bytes_from_init)
            }
        } else {
            let media_offset = local_offset - entry.init_len;
            self.read_media_segment(entry, media_offset, buf).await
        }
    }

    async fn read_media_segment(
        &self,
        entry: &SegmentEntry,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let assets = self.assets();
        let key = ResourceKey::from_url(&entry.segment_url);
        let resource: StreamingAssetResource = assets.open_streaming_resource(&key).await?;

        let read_end = (media_offset + buf.len() as u64).min(entry.media_len);
        resource.wait_range(media_offset..read_end).await?;

        let bytes_read = resource.read_at(media_offset, buf).await?;
        Ok(bytes_read)
    }
}

#[async_trait]
impl Source for HlsSource {
    type Item = u8;
    type Error = HlsError;

    async fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, HlsError> {
        debug!(
            range_start = range.start,
            range_end = range.end,
            "wait_range called"
        );

        // Wait until the range is covered by loaded segments or EOF.
        // Register notified() BEFORE checking data to avoid race with Downloader.
        loop {
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);

            {
                let segments = self.shared.segments.lock();
                let eof = self.shared.eof.load(Ordering::Acquire);
                let total = segments.total_bytes();

                if segments.range_covered(&range) {
                    debug!(range_start = range.start, "wait_range: covered");
                    return Ok(WaitOutcome::Ready);
                }

                if segments.find_at_offset(range.start).is_some() {
                    debug!(range_start = range.start, "wait_range: partial coverage");
                    return Ok(WaitOutcome::Ready);
                }

                if eof && range.start >= total {
                    debug!(range_start = range.start, total, "wait_range: EOF");
                    return Ok(WaitOutcome::Eof);
                }
            }

            // Signal downloader that reader needs data
            self.shared.reader_advanced.notify_one();

            // Wait for downloader to push more segments
            notified.await;
        }
    }

    async fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, HlsError> {
        debug!(offset, len = buf.len(), "read_at called");

        let entry = {
            let segments = self.shared.segments.lock();
            segments.find_at_offset(offset).cloned()
        };

        let Some(entry) = entry else {
            debug!(offset, "read_at: no entry at offset, returning 0");
            return Ok(0);
        };

        let bytes = self
            .read_from_entry(&entry, offset, buf)
            .await
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

        debug!(offset, bytes, "read_at complete");
        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        let segments = self.shared.segments.lock();
        if segments.is_empty() {
            return None;
        }
        Some(segments.total_bytes())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let segments = self.shared.segments.lock();
        let last = segments.last()?;
        Some(MediaInfo::new(last.codec, last.container))
    }

    fn current_segment_range(&self) -> Range<u64> {
        let segments = self.shared.segments.lock();
        match segments.last() {
            Some(entry) => entry.byte_offset..entry.end_offset(),
            None => 0..0,
        }
    }
}

/// Build an HlsDownloader + HlsSource pair from config.
pub fn build_pair(
    fetch: Arc<DefaultFetchManager>,
    variant_metadata: Vec<VariantMetadata>,
    config: &crate::options::HlsConfig,
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
    };

    let source = HlsSource {
        fetch,
        shared,
        events_tx: config.events_tx.clone(),
    };

    (downloader, source)
}
