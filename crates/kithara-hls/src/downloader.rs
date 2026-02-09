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
    source::{SegmentEntry, SharedSegments},
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
    /// Max segments to download in parallel per step.
    pub(crate) prefetch_count: usize,
}

/// Result of a segment download (network I/O only, no state mutation).
struct SegmentDownload {
    init_len: u64,
    init_url: Option<Url>,
    media: SegmentMeta,
    segment_index: usize,
    variant: usize,
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

        // Fire all HEAD requests in parallel (init + all media segments).
        let init_fut = async {
            if let Some(ref url) = init_url {
                fetch.get_content_length(url).await.unwrap_or(0)
            } else {
                0
            }
        };

        let media_futs: Vec<_> = media_urls
            .iter()
            .map(|url| fetch.get_content_length(url))
            .collect();

        let (init_len, media_lengths) =
            futures::future::join(init_fut, futures::future::join_all(media_futs)).await;

        let mut total = 0u64;
        let mut metadata = Vec::new();
        let mut byte_offset = 0u64;

        for (index, result) in media_lengths.into_iter().enumerate() {
            if let Ok(media_len) = result {
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

    /// Download media + init segment in parallel (network I/O only, no state mutation).
    async fn download_segment(
        fetch: Arc<DefaultFetchManager>,
        variant: usize,
        segment_index: usize,
        need_init: bool,
    ) -> Result<SegmentDownload, HlsError> {
        let init_fut = {
            let fetch = Arc::clone(&fetch);
            async move {
                if need_init {
                    match fetch.load_segment(variant, usize::MAX).await {
                        Ok(m) => (Some(m.url), m.len),
                        Err(_) => (None, 0),
                    }
                } else {
                    (None, 0)
                }
            }
        };

        let (media_result, (init_url, init_len)) =
            tokio::join!(fetch.load_segment(variant, segment_index), init_fut,);

        Ok(SegmentDownload {
            init_len,
            init_url,
            media: media_result?,
            segment_index,
            variant,
        })
    }

    /// Commit a downloaded segment to the segment index. Must be called sequentially.
    ///
    /// `download_duration` is used only for the `SegmentComplete` event.
    /// Throughput recording is the caller's responsibility.
    fn commit_download(
        &mut self,
        dl: SegmentDownload,
        is_variant_switch: bool,
        is_midstream_switch: bool,
        download_duration: Duration,
    ) {
        self.emit_event(HlsEvent::SegmentComplete {
            variant: dl.variant,
            segment_index: dl.segment_index,
            bytes_transferred: dl.media.len,
            duration: download_duration,
        });

        if is_variant_switch {
            self.sent_init_for_variant.insert(dl.variant);
        }

        let (byte_offset, actual_init_len) = if is_midstream_switch {
            (self.byte_offset, dl.init_len)
        } else if self.had_midstream_switch {
            (self.byte_offset, 0)
        } else {
            let offset = {
                let metadata_map = self.shared.variant_metadata.lock();
                if let Some(metadata) = metadata_map.get(&dl.variant)
                    && let Some(seg_meta) = metadata.get(dl.segment_index)
                {
                    seg_meta.byte_offset
                } else {
                    self.byte_offset
                }
            };
            let init = if is_variant_switch { dl.init_len } else { 0 };
            (offset, init)
        };

        let variant_codec = self.variants[dl.variant]
            .codec
            .as_ref()
            .and_then(|c| c.audio_codec);

        let media_len = dl.media.len;
        let entry = SegmentEntry {
            byte_offset,
            codec: variant_codec,
            init_len: actual_init_len,
            init_url: dl.init_url,
            meta: dl.media,
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
                segments.fence_at(byte_offset, dl.variant);
            }
            segments.push(entry);
        }
        self.shared.condvar.notify_all();
    }

    /// Prepare variant for download: detect switches, calculate metadata, populate cache.
    ///
    /// Returns `(is_variant_switch, is_midstream_switch)`.
    async fn ensure_variant_ready(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Result<(bool, bool), HlsError> {
        let is_variant_switch = !self.sent_init_for_variant.contains(&variant);
        let is_midstream_switch = is_variant_switch && segment_index > 0;

        if is_midstream_switch {
            self.had_midstream_switch = true;
            self.shared.segments.lock().had_midstream_switch = true;
            while self.shared.segment_requests.pop().is_some() {}
        }

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

                        if cached_count > 0 {
                            if cached_end_offset > self.byte_offset {
                                self.byte_offset = cached_end_offset;
                            }
                            if cached_count > self.current_segment_index {
                                self.current_segment_index = cached_count;
                            }
                            self.sent_init_for_variant.insert(variant);
                        }

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

        Ok((is_variant_switch, is_midstream_switch))
    }

    /// Fetch a single segment by variant and index.
    ///
    /// Used for on-demand requests (seek). For sequential downloads, `step()`
    /// uses `download_segment` + `commit_download` directly with batching.
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

        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(variant, segment_index)
        {
            debug!(variant, segment_index, "segment already loaded, skipping");
            return Ok(());
        }

        let (is_variant_switch, is_midstream_switch) =
            self.ensure_variant_ready(variant, segment_index).await?;

        // After a midstream switch, skip segments from the old variant.
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

        let start = Instant::now();
        let dl = Self::download_segment(
            Arc::clone(&self.fetch),
            variant,
            segment_index,
            true, // always try init for single-segment fetch
        )
        .await?;
        let duration = start.elapsed();

        self.record_throughput(dl.media.len, duration, dl.media.duration);
        self.commit_download(dl, is_variant_switch, is_midstream_switch, duration);
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

        // On-demand requests (seek): process immediately, single segment.
        if let Some(req) = self.shared.segment_requests.pop() {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                "processing on-demand segment request"
            );

            return match self.fetch_segment(req.variant, req.segment_index).await {
                Ok(()) => {
                    self.shared.condvar.notify_all();
                    if req.segment_index == self.current_segment_index {
                        self.current_segment_index += 1;
                    }
                    true
                }
                Err(e) => {
                    self.shared.condvar.notify_all();
                    debug!(?e, "segment fetch error");
                    self.emit_event(HlsEvent::DownloadError {
                        error: e.to_string(),
                    });
                    false
                }
            };
        }

        // Sequential batch download: ABR → prepare → download N in parallel → commit in order.
        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let variant = self.abr.get_current_variant_index();

        let num_segments = self.fetch.num_segments(variant).await.ok().unwrap_or(0);

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
                to_variant: variant,
                reason: decision.reason,
            });
        }

        // Prepare variant (metadata calc, variant-switch setup).
        let (is_variant_switch, is_midstream_switch) = match self
            .ensure_variant_ready(variant, self.current_segment_index)
            .await
        {
            Ok(flags) => flags,
            Err(e) => {
                self.shared.condvar.notify_all();
                debug!(?e, "variant preparation error");
                self.emit_event(HlsEvent::DownloadError {
                    error: e.to_string(),
                });
                return false;
            }
        };

        // Sequential batch: download → record → commit for up to prefetch_count segments.
        // Each segment goes through the full chain individually, so throughput
        // recording is identical to single-segment mode. The batch just reduces
        // per-step overhead (ABR decision, backpressure check, variant prep).
        let batch_end = (self.current_segment_index + self.prefetch_count).min(num_segments);
        let mut need_init = is_variant_switch;
        let mut downloaded = 0usize;

        for seg_idx in self.current_segment_index..batch_end {
            // Abort batch if on-demand request (seek) arrived.
            if !self.shared.segment_requests.is_empty() {
                break;
            }

            if self
                .shared
                .segments
                .lock()
                .is_segment_loaded(variant, seg_idx)
            {
                self.current_segment_index = seg_idx + 1;
                continue;
            }

            // Skip stale segments from old variant after midstream switch.
            if self.had_midstream_switch && !is_midstream_switch {
                let current = self.abr.get_current_variant_index();
                if variant != current {
                    continue;
                }
            }

            self.emit_event(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: self.byte_offset,
            });

            let start = Instant::now();
            let dl =
                match Self::download_segment(Arc::clone(&self.fetch), variant, seg_idx, need_init)
                    .await
                {
                    Ok(dl) => dl,
                    Err(e) => {
                        self.shared.condvar.notify_all();
                        debug!(?e, "segment fetch error");
                        self.emit_event(HlsEvent::DownloadError {
                            error: e.to_string(),
                        });
                        return false;
                    }
                };
            let duration = start.elapsed();

            self.record_throughput(dl.media.len, duration, dl.media.duration);
            self.commit_download(
                dl,
                is_variant_switch && downloaded == 0,
                is_midstream_switch && downloaded == 0,
                duration,
            );

            self.current_segment_index = seg_idx + 1;
            need_init = false;
            downloaded += 1;
        }

        if downloaded == 0 {
            // All segments in window already loaded (from cache).
            self.current_segment_index = batch_end;
            self.shared.condvar.notify_all();
        }

        // Yield periodically to let other tasks run (e.g., playback).
        if self.look_ahead_bytes.is_none() {
            self.segments_since_yield += downloaded.max(1);
            if self.segments_since_yield >= self.yield_interval {
                self.segments_since_yield = 0;
                tokio::task::yield_now().await;
            }
        }

        true
    }
}
