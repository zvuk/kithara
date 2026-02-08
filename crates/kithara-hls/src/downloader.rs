//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use kithara_abr::{AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource};
use kithara_assets::{Assets, ResourceKey};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{ContainerFormat, Downloader};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    events::HlsEvent,
    fetch::{DefaultFetchManager, Loader, SegmentMeta, SegmentType},
    parsing::VariantStream,
    source::{SegmentEntry, SegmentRequest, SharedSegments},
};

/// Metadata for a single segment in a variant's theoretical stream.
#[derive(Debug, Clone)]
pub(crate) struct SegmentMetadata {
    /// Segment index in playlist.
    #[expect(dead_code)]
    pub(crate) index: usize,
    /// Byte offset in theoretical stream (cumulative from segment 0).
    pub(crate) byte_offset: u64,
    /// Total size (init + media) in bytes.
    pub(crate) size: u64,
}

/// HLS downloader: fetches segments and maintains ABR state.
pub struct HlsDownloader {
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) variants: Vec<VariantStream>,
    pub(crate) current_segment_index: usize,
    pub(crate) sent_init_for_variant: HashSet<usize>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) byte_offset: u64,
    pub(crate) shared: Arc<SharedSegments>,
    pub(crate) events_tx: Option<broadcast::Sender<HlsEvent>>,
    pub(crate) cancel: CancellationToken,
    /// Backpressure threshold. None = no backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// How often to yield to async runtime (every N segments).
    pub(crate) yield_interval: usize,
    /// Counter for yield interval.
    pub(crate) segments_since_yield: usize,
    /// Cache of `expected_total_length` per variant (`variant_index` → `total_bytes`).
    pub(crate) variant_lengths: HashMap<usize, u64>,
    /// True after a mid-stream variant switch. Subsequent segments use cumulative offsets.
    pub(crate) had_midstream_switch: bool,
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
    #[expect(clippy::too_many_arguments)]
    fn populate_cached_segments(
        shared: &SharedSegments,
        fetch: &DefaultFetchManager,
        variant: usize,
        metadata: &[SegmentMetadata],
        init_url: &Option<Url>,
        init_len: u64,
        media_urls: &[Url],
        variant_stream: &VariantStream,
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

        let variant_codec = variant_stream.codec.as_ref().and_then(|c| c.audio_codec);

        let mut segments = shared.segments.lock();
        let mut count = 0usize;
        let mut final_offset = 0u64;

        for (index, (meta, segment_url)) in metadata.iter().zip(media_urls.iter()).enumerate() {
            let key = ResourceKey::from_url(segment_url);
            let Ok(resource) = assets.open_resource(&key) else {
                break; // Stop on first uncached segment
            };

            if let ResourceStatus::Committed { final_len } = resource.status() {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                // Init only for the first segment (init-once layout matches metadata)
                let actual_init_len = if count == 0 { init_len } else { 0 };

                let entry = SegmentEntry {
                    byte_offset: meta.byte_offset,
                    codec: variant_codec,
                    init_len: actual_init_len,
                    init_url: init_url.clone(),
                    meta: SegmentMeta {
                        variant,
                        segment_type: SegmentType::Media(index),
                        sequence: 0,
                        url: segment_url.clone(),
                        duration: None,
                        key: None,
                        len: media_len,
                        container,
                    },
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
                            let vstream = &self.variants[variant];
                            Self::populate_cached_segments(
                                &self.shared,
                                &self.fetch,
                                variant,
                                &metadata,
                                &init_url,
                                init_len,
                                &media_urls,
                                vstream,
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
                            let switch_meta =
                                metadata.get(segment_index).map_or(0, |s| s.byte_offset);
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

        let variant_codec = self.variants[variant]
            .codec
            .as_ref()
            .and_then(|c| c.audio_codec);

        let media_len = meta.len;
        let entry = SegmentEntry {
            byte_offset,
            codec: variant_codec,
            init_len: actual_init_len,
            init_url,
            meta,
        };

        let end = byte_offset + actual_init_len + media_len;
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
        // Backpressure: wait if too far ahead of reader (only when limit is set).
        // On-demand requests (from wait_range during seek/atom scan) bypass
        // backpressure to prevent deadlock: reader blocked in condvar.wait()
        // can't advance reader_pos, so the gap never shrinks.
        if let Some(limit) = self.look_ahead_bytes {
            loop {
                if !self.shared.segment_requests.is_empty() {
                    break;
                }

                let advanced = self.shared.reader_advanced.notified();
                tokio::pin!(advanced);

                let reader_pos = self
                    .shared
                    .reader_offset
                    .load(std::sync::atomic::Ordering::Acquire);
                let downloaded = self.shared.segments.lock().total_bytes();

                if downloaded.saturating_sub(reader_pos) <= limit {
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
                self.shared
                    .eof
                    .store(true, std::sync::atomic::Ordering::Release);
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
                segment_index: self.current_segment_index,
                variant: next_variant,
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

                    // Yield periodically to let other tasks run (e.g., playback).
                    // Only needed when no backpressure (look_ahead_bytes is None).
                    if self.look_ahead_bytes.is_none() {
                        self.segments_since_yield += 1;
                        if self.segments_since_yield >= self.yield_interval {
                            self.segments_since_yield = 0;
                            tokio::task::yield_now().await;
                        }
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
