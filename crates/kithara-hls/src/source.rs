//! Unified HLS source implementing kithara_stream::Source.
//!
//! Combines segment fetching and random-access reading in one component.
//! Eliminates the double-layer architecture (worker + adapter).

use std::{
    collections::HashSet,
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use kithara_abr::{
    AbrController, AbrMode, AbrOptions, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use kithara_assets::{AssetStore, Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Source, StreamError, StreamResult};
use tokio::sync::broadcast;
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

/// Unified HLS source.
///
/// Fetches segments on demand and provides random-access reading.
/// Implements `kithara_stream::Source` directly.
pub struct HlsSource {
    fetch: Arc<DefaultFetchManager>,
    variant_metadata: Vec<VariantMetadata>,
    current_segment_index: usize,
    sent_init_for_variant: HashSet<usize>,
    abr: AbrController<ThroughputEstimator>,
    byte_offset: u64,
    segments: SegmentIndex,
    eof_reached: bool,
    events_tx: Option<broadcast::Sender<HlsEvent>>,
    cancel: CancellationToken,
}

impl HlsSource {
    /// Create new HLS source.
    pub fn new(
        fetch: Arc<DefaultFetchManager>,
        variant_metadata: Vec<VariantMetadata>,
        initial_variant: usize,
        abr_options: Option<AbrOptions>,
        events_tx: Option<broadcast::Sender<HlsEvent>>,
        cancel: CancellationToken,
    ) -> Self {
        // Build variants from metadata
        let variants: Vec<Variant> = variant_metadata
            .iter()
            .map(|v| Variant {
                variant_index: v.index,
                bandwidth_bps: v.bitrate.unwrap_or(0),
            })
            .collect();

        // Merge variants into ABR options
        let mut abr_opts = abr_options.unwrap_or_else(|| AbrOptions {
            mode: AbrMode::Manual(initial_variant),
            ..AbrOptions::default()
        });
        abr_opts.variants = variants;

        let abr = AbrController::new(abr_opts);

        Self {
            fetch,
            variant_metadata,
            current_segment_index: 0,
            sent_init_for_variant: HashSet::new(),
            abr,
            byte_offset: 0,
            segments: SegmentIndex::new(),
            eof_reached: false,
            events_tx,
            cancel,
        }
    }

    /// Get asset store.
    pub fn assets(&self) -> AssetStore {
        self.fetch.assets().clone()
    }

    /// Subscribe to events.
    #[allow(dead_code)]
    pub fn events(&self) -> Option<broadcast::Receiver<HlsEvent>> {
        self.events_tx.as_ref().map(|tx| tx.subscribe())
    }

    fn emit_event(&self, event: HlsEvent) {
        if let Some(ref tx) = self.events_tx {
            let _ = tx.send(event);
        }
    }

    /// Fetch next segment and add to index. Returns true if EOF.
    async fn fetch_next_segment(&mut self) -> Result<bool, HlsError> {
        if self.cancel.is_cancelled() {
            debug!("cancelled, returning EOF");
            return Ok(true);
        }

        // ABR: decide which variant to use for next segment
        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let current_variant = self.abr.get_current_variant_index();

        // Check if variant changed (for init segment handling and events)
        let is_variant_switch = !self.sent_init_for_variant.contains(&current_variant);

        let num_segments = self.fetch.num_segments(current_variant).await?;

        if self.current_segment_index >= num_segments {
            debug!("reached end of playlist");
            self.emit_event(HlsEvent::EndOfStream);
            return Ok(true);
        }

        // Emit segment start event
        self.emit_event(HlsEvent::SegmentStart {
            variant: current_variant,
            segment_index: self.current_segment_index,
            byte_offset: self.byte_offset,
        });

        // Measure download time for throughput estimation
        let fetch_start = Instant::now();

        let meta = self
            .fetch
            .load_segment(current_variant, self.current_segment_index)
            .await?;

        let fetch_duration = fetch_start.elapsed();

        // Record throughput sample for ABR
        self.record_throughput(meta.len, fetch_duration, meta.duration);

        // Emit segment complete event
        self.emit_event(HlsEvent::SegmentComplete {
            variant: current_variant,
            segment_index: self.current_segment_index,
            bytes_transferred: meta.len,
            duration: fetch_duration,
        });

        // Emit variant applied event if changed
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

        // Update state
        self.byte_offset += actual_init_len + meta.len;
        self.current_segment_index += 1;
        self.segments.push(entry);

        Ok(false)
    }

    /// Make ABR decision and apply if variant changed.
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

    /// Record throughput sample for ABR.
    fn record_throughput(
        &mut self,
        bytes: u64,
        duration: Duration,
        content_duration: Option<Duration>,
    ) {
        // Skip very short downloads (likely cached)
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

        // Emit throughput event
        let bytes_per_second = if duration.as_secs_f64() > 0.0 {
            bytes as f64 / duration.as_secs_f64()
        } else {
            0.0
        };
        self.emit_event(HlsEvent::ThroughputSample { bytes_per_second });
    }

    /// Ensure segments cover the given range.
    async fn ensure_range(&mut self, range: Range<u64>) -> Result<(), HlsError> {
        loop {
            if self.eof_reached {
                debug!(
                    range_start = range.start,
                    range_end = range.end,
                    "ensure_range: eof already reached"
                );
                return Ok(());
            }

            if self.segments.range_covered(&range) {
                return Ok(());
            }

            debug!(
                range_start = range.start,
                range_end = range.end,
                current_segment_index = self.current_segment_index,
                total_bytes = self.segments.total_bytes(),
                "ensure_range: fetching next segment"
            );

            let is_eof = self.fetch_next_segment().await?;
            if is_eof {
                debug!("ensure_range: EOF reached after fetch");
                self.eof_reached = true;
                return Ok(());
            }
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

        self.ensure_range(range.clone())
            .await
            .map_err(StreamError::Source)?;

        // Return Ready if ANY data is available at range.start.
        // The caller will read what's available and request more.
        // Only return Eof if we're past all loaded data AND stream is finished.
        let has_segment = self.segments.find_at_offset(range.start).is_some();
        let total = self.segments.total_bytes();

        let outcome = if has_segment {
            WaitOutcome::Ready
        } else if self.eof_reached && range.start >= total {
            WaitOutcome::Eof
        } else {
            WaitOutcome::Ready
        };

        debug!(
            range_start = range.start,
            ?outcome,
            has_segment,
            total,
            eof_reached = self.eof_reached,
            "wait_range result"
        );

        Ok(outcome)
    }

    async fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, HlsError> {
        let range = offset..offset + buf.len() as u64;
        debug!(offset, range_end = range.end, "read_at called");

        self.ensure_range(range)
            .await
            .map_err(StreamError::Source)?;

        let entry = self.segments.find_at_offset(offset).cloned();

        let Some(entry) = entry else {
            debug!(
                offset,
                total_bytes = self.segments.total_bytes(),
                eof_reached = self.eof_reached,
                "read_at: no entry found at offset, returning 0"
            );
            return Ok(0);
        };

        let bytes = self
            .read_from_entry(&entry, offset, buf)
            .await
            .map_err(StreamError::Source)?;

        debug!(offset, bytes, "read_at complete");
        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        if self.segments.is_empty() {
            return None;
        }
        Some(self.segments.total_bytes())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let last = self.segments.last()?;
        Some(MediaInfo::new(last.codec, last.container))
    }

    fn current_segment_range(&self) -> Range<u64> {
        match self.segments.last() {
            Some(entry) => entry.byte_offset..entry.end_offset(),
            None => 0..0,
        }
    }
}
