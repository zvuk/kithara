//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use kithara_abr::{AbrController, ThroughputEstimator, ThroughputSample, ThroughputSampleSource};
use kithara_assets::{CoverageIndex, DiskCoverage, ResourceKey};
use kithara_storage::{Coverage, MmapResource, ResourceExt, ResourceStatus};
use kithara_stream::{Downloader, DownloaderIo, PlanOutcome};
use tokio::sync::broadcast;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    download_state::{DownloadProgress, LoadedSegment},
    events::HlsEvent,
    fetch::{DefaultFetchManager, Loader, SegmentMeta},
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
    source::SharedSegments,
};

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
                    match fetch.load_init_segment(plan.variant).await {
                        Ok(m) => (Some(m.url), m.len),
                        Err(e) => {
                            tracing::warn!(
                                variant = plan.variant,
                                error = %e,
                                "init segment load failed"
                            );
                            (None, 0)
                        }
                    }
                } else {
                    (None, 0)
                }
            }
        };

        let (media_result, (init_url, init_len)) = tokio::join!(
            self.fetch
                .load_media_segment(plan.variant, plan.segment_index),
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

/// HLS downloader: fetches segments and maintains ABR state.
pub struct HlsDownloader {
    pub(crate) io: HlsIo,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) current_segment_index: usize,
    pub(crate) sent_init_for_variant: HashSet<usize>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) byte_offset: u64,
    pub(crate) shared: Arc<SharedSegments>,
    pub(crate) events_tx: Option<broadcast::Sender<HlsEvent>>,
    /// Backpressure threshold. None = no backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
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

    /// Calculate size map for a variant via HEAD requests and store in `PlaylistState`.
    async fn calculate_size_map(
        playlist_state: &PlaylistState,
        fetch: &Arc<DefaultFetchManager>,
        variant: usize,
    ) -> Result<(), HlsError> {
        if playlist_state.has_size_map(variant) {
            return Ok(());
        }

        let init_url = playlist_state.init_url(variant);
        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);

        // HEAD for init
        let init_size = if let Some(ref url) = init_url {
            fetch.get_content_length(url).await.unwrap_or(0)
        } else {
            0
        };

        // HEAD for all media segments in parallel
        let media_urls: Vec<_> = (0..num_segments)
            .filter_map(|i| playlist_state.segment_url(variant, i))
            .collect();
        let media_futs: Vec<_> = media_urls
            .iter()
            .map(|url| fetch.get_content_length(url))
            .collect();
        let media_lengths = futures::future::join_all(media_futs).await;

        let mut offsets = Vec::with_capacity(num_segments);
        let mut segment_sizes = Vec::with_capacity(num_segments);
        let mut cumulative = 0u64;

        for (i, result) in media_lengths.into_iter().enumerate() {
            let media_len = result.unwrap_or(0);
            let total_seg = if i == 0 {
                init_size + media_len
            } else {
                media_len
            };
            offsets.push(cumulative);
            segment_sizes.push(total_seg);
            cumulative += total_seg;
        }

        debug!(
            variant,
            total = cumulative,
            num_segments = segment_sizes.len(),
            "calculated variant size map"
        );

        playlist_state.set_size_map(
            variant,
            VariantSizeMap {
                init_size,
                segment_sizes,
                offsets,
                total: cumulative,
            },
        );
        Ok(())
    }

    /// Pre-populate segment index with segments already committed on disk.
    ///
    /// Scans the asset store for committed segment resources and creates entries
    /// so that `loaded_ranges` reflects actual disk state. Uses cumulative offsets
    /// derived from actual (decrypted) resource sizes on disk.
    ///
    /// Init data is included only for the first segment (init-once layout).
    /// Stops at the first uncached segment to maintain contiguous entries.
    ///
    /// Returns `(count, cumulative_byte_offset)` for caller to update downloader state.
    fn populate_cached_segments(
        shared: &SharedSegments,
        fetch: &DefaultFetchManager,
        variant: usize,
        coverage_index: &Option<Arc<CoverageIndex<MmapResource>>>,
    ) -> (usize, u64) {
        // Ephemeral backend has no persistent cache to scan.
        if fetch.backend().is_ephemeral() {
            return (0, 0);
        }

        let backend = fetch.backend();
        let playlist_state = &shared.playlist_state;

        let init_url = playlist_state.init_url(variant);
        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);

        // If init is required, verify it's cached on disk
        if let Some(ref url) = init_url {
            let init_key = ResourceKey::from_url(url);
            let init_cached = backend
                .open_resource(&init_key)
                .map(|r| matches!(r.status(), ResourceStatus::Committed { .. }))
                .unwrap_or(false);
            if !init_cached {
                return (0, 0);
            }
        }

        // Get init_size from actual disk resource (size map was populated by calculate_size_map).
        let init_len = if playlist_state.total_variant_size(variant).is_some() {
            if let Some(ref url) = init_url {
                let key = ResourceKey::from_url(url);
                backend
                    .open_resource(&key)
                    .ok()
                    .and_then(|r| match r.status() {
                        ResourceStatus::Committed { final_len } => final_len,
                        _ => None,
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        let mut segments = shared.segments.lock();
        let mut count = 0usize;
        let mut cumulative_offset = 0u64;

        for index in 0..num_segments {
            let Some(segment_url) = playlist_state.segment_url(variant, index) else {
                break;
            };

            let key = ResourceKey::from_url(&segment_url);
            let Ok(resource) = backend.open_resource(&key) else {
                break; // Stop on first uncached segment
            };

            if let ResourceStatus::Committed { final_len } = resource.status() {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                // Validate against coverage if available.
                // No entry -> legacy file, treat as valid.
                // Entry exists but incomplete -> partially written, skip.
                if let Some(idx) = coverage_index {
                    let cov = DiskCoverage::open(Arc::clone(idx), segment_url.to_string());
                    if cov.total_size().is_some() && !cov.is_complete() {
                        break;
                    }
                }

                // Init only for the first segment (init-once layout matches metadata)
                let actual_init_len = if count == 0 { init_len } else { 0 };

                let segment = LoadedSegment {
                    variant,
                    segment_index: index,
                    byte_offset: cumulative_offset,
                    init_len: actual_init_len,
                    media_len,
                    init_url: init_url.clone(),
                    media_url: segment_url,
                };

                cumulative_offset = segment.end_offset();
                segments.push(segment);
                count += 1;
            } else {
                break; // Not committed, stop
            }
        }

        drop(segments);
        if count > 0 {
            debug!(
                variant,
                count, cumulative_offset, "pre-populated cached segments from disk"
            );
            shared.condvar.notify_all();
        }

        (count, cumulative_offset)
    }

    /// Commit a downloaded segment to the segment index.
    ///
    /// `is_variant_switch` / `is_midstream_switch` control init segment inclusion.
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

        let actual_init_len = if is_midstream_switch || is_variant_switch {
            dl.init_len
        } else {
            0
        };

        let had_midstream_switch = self.shared.had_midstream_switch.load(Ordering::Acquire);
        let byte_offset = if is_midstream_switch || had_midstream_switch {
            // Variant switch: always use cumulative offset.
            self.byte_offset
        } else {
            // Use metadata offset for correct positioning of both sequential
            // and on-demand (seek) loads. Metadata is kept in sync with actual
            // sizes via reconcile_metadata() after each commit.
            self.playlist_state
                .segment_byte_offset(dl.variant, dl.segment_index)
                .unwrap_or(self.byte_offset)
        };

        let media_len = dl.media.len;
        let actual_size = actual_init_len + media_len;

        let media_url = dl.media.url.clone();
        let segment = LoadedSegment {
            variant: dl.variant,
            segment_index: dl.segment_index,
            byte_offset,
            init_len: actual_init_len,
            media_len,
            init_url: dl.init_url,
            media_url: media_url.clone(),
        };

        let end = byte_offset + actual_size;
        if end > self.byte_offset {
            self.byte_offset = end;
        }

        // Reconcile metadata: update this segment's size and recalculate subsequent
        // byte_offsets using the actual (possibly decrypted) size. For non-DRM streams
        // this is a no-op (actual == predicted). For DRM streams, it corrects the drift
        // from PKCS7 padding removal so future lookups use accurate offsets.
        let pre_total = self.playlist_state.total_variant_size(dl.variant);
        self.playlist_state
            .reconcile_segment_size(dl.variant, dl.segment_index, actual_size);
        let post_total = self.playlist_state.total_variant_size(dl.variant);

        // Update expected_total_length if reconciliation changed the variant total.
        // This handles cases where actual sizes differ from HEAD predictions:
        // - Larger: HTTP auto-decompression returns more bytes than Content-Length
        // - Smaller: DRM decryption removes PKCS7 padding
        // Uses delta-based adjustment so midstream switch offsets are preserved.
        if let (Some(pre), Some(post)) = (pre_total, post_total)
            && pre != post
        {
            let current = self.shared.expected_total_length.load(Ordering::Acquire);
            let new_expected = if post > pre {
                current.saturating_add(post - pre)
            } else {
                current.saturating_sub(pre - post)
            };
            self.shared
                .expected_total_length
                .store(new_expected, Ordering::Release);
        }

        self.emit_event(HlsEvent::DownloadProgress {
            offset: self.byte_offset,
            total: None,
        });

        // Mark segment coverage for crash-safe tracking via DiskCoverage.
        {
            let mut segments = self.shared.segments.lock();
            if is_variant_switch {
                segments.fence_at(byte_offset, dl.variant);
            }
            segments.push(segment);
        }
        self.shared.condvar.notify_all();

        if let Some(ref idx) = self.coverage_index {
            let mut cov = DiskCoverage::open(Arc::clone(idx), media_url.to_string());
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
            self.shared
                .had_midstream_switch
                .store(true, Ordering::Release);
            while self.shared.segment_requests.pop().is_some() {}
        }

        let current_length = self.shared.expected_total_length.load(Ordering::Acquire);
        if is_variant_switch || current_length == 0 {
            let needs_calculation = !self.playlist_state.has_size_map(variant);

            if needs_calculation {
                match Self::calculate_size_map(&self.playlist_state, &self.fetch, variant).await {
                    Ok(()) => {
                        let total = self.playlist_state.total_variant_size(variant).unwrap_or(0);

                        debug!(variant, total, "calculated and cached variant size map");

                        let (cached_count, cached_end_offset) = if !is_variant_switch {
                            Self::populate_cached_segments(
                                &self.shared,
                                &self.fetch,
                                variant,
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
                            let switch_meta = self
                                .playlist_state
                                .segment_byte_offset(variant, segment_index)
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

                        if effective_total > 0 {
                            self.shared
                                .expected_total_length
                                .store(effective_total, Ordering::Release);
                        }
                    }
                    Err(e) => {
                        debug!(?e, variant, "failed to calculate variant size map");
                    }
                }
            } else {
                // Size map already exists, use cached total
                if let Some(cached_total) = self.playlist_state.total_variant_size(variant) {
                    debug!(variant, total = cached_total, "using cached variant length");
                    if cached_total > 0 {
                        self.shared
                            .expected_total_length
                            .store(cached_total, Ordering::Release);
                    }
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

impl Drop for HlsDownloader {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shared.condvar.notify_all();
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
        if self.shared.had_midstream_switch.load(Ordering::Acquire) && !is_midstream_switch {
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
            let had_switch = self.shared.had_midstream_switch.load(Ordering::Acquire);
            if had_switch && !is_midstream_switch {
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

    fn commit(&mut self, fetch: HlsFetch) {
        let is_variant_switch = !self.sent_init_for_variant.contains(&fetch.variant);
        let is_midstream_switch = is_variant_switch && fetch.segment_index > 0;

        // Only advance sequential position for the next expected segment.
        // On-demand loads and out-of-order batch results must not jump past gaps --
        // plan() handles skipping loaded segments when building the next batch.
        if fetch.segment_index == self.current_segment_index {
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
        let downloaded = self.shared.segments.lock().max_end_offset();

        downloaded.saturating_sub(reader_pos) > limit
    }

    async fn wait_ready(&self) {
        self.shared.reader_advanced.notified().await;
    }
}
