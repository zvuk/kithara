//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use kithara_abr::{AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource};
use kithara_assets::{CoverageIndex, DiskCoverage, ResourceKey};
use kithara_storage::{Coverage, MmapResource, ResourceExt, ResourceStatus};
use kithara_stream::{ContainerFormat, Downloader, DownloaderIo, PlanOutcome, StepResult};
use tokio::sync::broadcast;
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

// HlsIo — pure I/O executor (Clone, no &mut self)

/// Pure I/O executor for HLS segment fetching.
#[derive(Clone)]
pub struct HlsIo {
    fetch: Arc<DefaultFetchManager>,
}

impl HlsIo {
    pub(crate) fn new(fetch: Arc<DefaultFetchManager>) -> Self {
        Self { fetch }
    }
}

/// Plan for downloading a single HLS segment.
pub struct HlsPlan {
    pub(crate) variant: usize,
    pub(crate) segment_index: usize,
    pub(crate) need_init: bool,
}

/// Result of downloading a single HLS segment.
pub struct HlsFetch {
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<Url>,
    pub(crate) media: SegmentMeta,
    pub(crate) segment_index: usize,
    pub(crate) variant: usize,
    pub(crate) duration: Duration,
}

impl DownloaderIo for HlsIo {
    type Plan = HlsPlan;
    type Fetch = HlsFetch;
    type Error = HlsError;

    async fn fetch(&self, plan: HlsPlan) -> Result<HlsFetch, HlsError> {
        let start = Instant::now();

        let init_fut = {
            let fetch = Arc::clone(&self.fetch);
            async move {
                if plan.need_init {
                    match fetch.load_segment(plan.variant, usize::MAX).await {
                        Ok(m) => (Some(m.url), m.len),
                        Err(_) => (None, 0),
                    }
                } else {
                    (None, 0)
                }
            }
        };

        let (media_result, (init_url, init_len)) = tokio::join!(
            self.fetch.load_segment(plan.variant, plan.segment_index),
            init_fut,
        );

        let duration = start.elapsed();

        Ok(HlsFetch {
            init_len,
            init_url,
            media: media_result?,
            segment_index: plan.segment_index,
            variant: plan.variant,
            duration,
        })
    }
}

// HlsDownloader — mutable state (plan + commit)

/// HLS downloader: fetches segments and maintains ABR state.
pub struct HlsDownloader {
    pub(crate) io: HlsIo,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) variants: Vec<VariantStream>,
    pub(crate) current_segment_index: usize,
    pub(crate) sent_init_for_variant: HashSet<usize>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) byte_offset: u64,
    pub(crate) shared: Arc<SharedSegments>,
    pub(crate) events_tx: Option<broadcast::Sender<HlsEvent>>,
    /// Backpressure threshold. None = no backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Cache of `expected_total_length` per variant (`variant_index` → `total_bytes`).
    pub(crate) variant_lengths: HashMap<usize, u64>,
    /// True after a mid-stream variant switch. Subsequent segments use cumulative offsets.
    pub(crate) had_midstream_switch: bool,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
    /// Coverage index for crash-safe segment tracking.
    pub(crate) coverage_index: Option<Arc<CoverageIndex<MmapResource>>>,
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
        coverage_index: &Option<Arc<CoverageIndex<MmapResource>>>,
    ) -> (usize, u64) {
        // Ephemeral backend has no persistent cache to scan.
        if fetch.backend().is_ephemeral() {
            return (0, 0);
        }

        let backend = fetch.backend();
        let container = if init_url.is_some() {
            Some(ContainerFormat::Fmp4)
        } else {
            Some(ContainerFormat::MpegTs)
        };

        // If init is required, verify it's cached on disk
        if let Some(url) = init_url {
            let init_key = ResourceKey::from_url(url);
            let init_cached = backend
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
            let Ok(resource) = backend.open_resource(&key) else {
                break; // Stop on first uncached segment
            };

            if let ResourceStatus::Committed { final_len } = resource.status() {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                // Validate against coverage if available.
                // No entry → legacy file, treat as valid.
                // Entry exists but incomplete → partially written, skip.
                if let Some(idx) = coverage_index {
                    let cov = DiskCoverage::open(Arc::clone(idx), segment_url.to_string());
                    if cov.total_size().is_some() && !cov.is_complete() {
                        break;
                    }
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

    /// Commit a downloaded segment to the segment index.
    ///
    /// `is_variant_switch` / `is_midstream_switch` control offset calculation.
    fn commit_segment(&mut self, dl: HlsFetch, is_variant_switch: bool, is_midstream_switch: bool) {
        self.record_throughput(dl.media.len, dl.duration, dl.media.duration);

        self.emit_event(HlsEvent::SegmentComplete {
            variant: dl.variant,
            segment_index: dl.segment_index,
            bytes_transferred: dl.media.len,
            duration: dl.duration,
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

        // Mark segment coverage for crash-safe tracking via DiskCoverage.
        let segment_url = entry.meta.url.clone();
        {
            let mut segments = self.shared.segments.lock();
            if is_variant_switch {
                segments.fence_at(byte_offset, dl.variant);
            }
            segments.push(entry);
        }
        self.shared.condvar.notify_all();

        if let Some(ref idx) = self.coverage_index {
            let mut cov = DiskCoverage::open(Arc::clone(idx), segment_url.to_string());
            cov.set_total_size(media_len);
            cov.mark(0..media_len);
            // flush happens via Drop, or explicitly in commit()
        }
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
                                &self.coverage_index,
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
    type Plan = HlsPlan;
    type Fetch = HlsFetch;
    type Error = HlsError;
    type Io = HlsIo;

    fn io(&self) -> &Self::Io {
        &self.io
    }

    async fn poll_demand(&mut self) -> Option<HlsPlan> {
        let req = self.shared.segment_requests.pop()?;

        debug!(
            variant = req.variant,
            segment_index = req.segment_index,
            "processing on-demand segment request"
        );

        let num_segments = self.fetch.num_segments(req.variant).await.ok().unwrap_or(0);
        if req.segment_index >= num_segments {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                num_segments,
                "segment index out of range"
            );
            self.shared.condvar.notify_all();
            return None;
        }

        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(req.variant, req.segment_index)
        {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                "segment already loaded, skipping"
            );
            self.shared.condvar.notify_all();
            return None;
        }

        // Prepare variant (metadata, variant switch detection).
        let (is_variant_switch, is_midstream_switch) = match self
            .ensure_variant_ready(req.variant, req.segment_index)
            .await
        {
            Ok(flags) => flags,
            Err(e) => {
                debug!(?e, "variant preparation error in poll_demand");
                self.shared.condvar.notify_all();
                self.emit_event(HlsEvent::DownloadError {
                    error: e.to_string(),
                });
                return None;
            }
        };

        // Skip stale segments from old variant after midstream switch.
        if self.had_midstream_switch && !is_midstream_switch {
            let current_variant = self.abr.get_current_variant_index();
            if req.variant != current_variant {
                debug!(
                    variant = req.variant,
                    segment_index = req.segment_index,
                    current_variant,
                    "skipping stale segment from pre-switch variant"
                );
                self.shared.condvar.notify_all();
                return None;
            }
        }

        // Re-check after metadata calculation (populate_cached_segments may have loaded it).
        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(req.variant, req.segment_index)
        {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                "segment loaded from cache after metadata calc"
            );
            self.shared.condvar.notify_all();
            return None;
        }

        self.emit_event(HlsEvent::SegmentStart {
            variant: req.variant,
            segment_index: req.segment_index,
            byte_offset: self.byte_offset,
        });

        Some(HlsPlan {
            variant: req.variant,
            segment_index: req.segment_index,
            need_init: is_variant_switch || !self.sent_init_for_variant.contains(&req.variant),
        })
    }

    async fn plan(&mut self) -> PlanOutcome<HlsPlan> {
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
            return PlanOutcome::Complete;
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
                return PlanOutcome::Complete;
            }
        };

        // Build batch of plans.
        let batch_end = (self.current_segment_index + self.prefetch_count).min(num_segments);
        let mut plans = Vec::new();
        let mut need_init = is_variant_switch;

        for seg_idx in self.current_segment_index..batch_end {
            // Skip already loaded segments.
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

            plans.push(HlsPlan {
                variant,
                segment_index: seg_idx,
                need_init,
            });
            need_init = false;
        }

        if plans.is_empty() {
            // All segments in window already loaded (from cache).
            self.current_segment_index = batch_end;
            self.shared.condvar.notify_all();
        }

        PlanOutcome::Batch(plans)
    }

    async fn step(&mut self) -> Result<StepResult<HlsFetch>, HlsError> {
        // HLS does not use streaming step — all work goes through plan/batch.
        std::future::pending().await
    }

    fn commit(&mut self, fetch: HlsFetch) {
        let is_variant_switch = !self.sent_init_for_variant.contains(&fetch.variant);
        let is_midstream_switch = is_variant_switch && fetch.segment_index > 0;

        // Advance current_segment_index if this is the next expected segment.
        if fetch.segment_index == self.current_segment_index {
            self.current_segment_index = fetch.segment_index + 1;
        } else if fetch.segment_index >= self.current_segment_index {
            // Batch may commit out of order — advance past if needed.
            self.current_segment_index = fetch.segment_index + 1;
        }

        self.commit_segment(fetch, is_variant_switch, is_midstream_switch);
    }

    fn should_throttle(&self) -> bool {
        let Some(limit) = self.look_ahead_bytes else {
            return false;
        };

        let reader_pos = self
            .shared
            .reader_offset
            .load(std::sync::atomic::Ordering::Acquire);
        let downloaded = self.shared.segments.lock().total_bytes();

        downloaded.saturating_sub(reader_pos) > limit
    }

    async fn wait_ready(&self) {
        self.shared.reader_advanced.notified().await;
    }
}
