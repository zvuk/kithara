//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use kithara_abr::{
    AbrController, AbrDecision, AbrReason, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource,
};
use kithara_assets::ResourceKey;
use kithara_coverage::{Coverage, CoverageManager};
use kithara_events::{EventBus, HlsEvent, SeekEpoch};
use kithara_platform::time::Instant;
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
use kithara_stream::{Downloader, DownloaderIo, PlanOutcome};
use tokio::task::yield_now;
use tracing::debug;
use url::Url;

use crate::{
    HlsError,
    download_state::{DownloadProgress, DownloadState, LoadedSegment},
    fetch::{DefaultFetchManager, Loader, SegmentMeta},
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
    source::{SegmentRequest, SharedSegments},
};

fn is_stale_epoch(fetch_epoch: SeekEpoch, current_epoch: SeekEpoch) -> bool {
    fetch_epoch != current_epoch
}

fn is_cross_codec_switch(
    playlist_state: &PlaylistState,
    from_variant: usize,
    to_variant: usize,
) -> bool {
    matches!(
        (
            playlist_state.variant_codec(from_variant),
            playlist_state.variant_codec(to_variant),
        ),
        (Some(from_codec), Some(to_codec)) if from_codec != to_codec
    )
}

fn first_missing_segment(
    state: &DownloadState,
    variant: usize,
    start_segment: usize,
    num_segments: usize,
) -> Option<usize> {
    let start = start_segment.min(num_segments);
    (start..num_segments).find(|&segment_index| !state.is_segment_loaded(variant, segment_index))
}

fn classify_variant_transition(
    last_committed_variant: Option<usize>,
    sent_init_for_variant: &HashSet<usize>,
    variant: usize,
    segment_index: usize,
) -> (bool, bool) {
    let variant_changed = last_committed_variant.is_some_and(|previous| previous != variant);
    let is_initial_start =
        last_committed_variant.is_none() && !sent_init_for_variant.contains(&variant);
    let is_variant_switch = variant_changed || is_initial_start;
    let is_midstream_switch = variant_changed && segment_index > 0;
    (is_variant_switch, is_midstream_switch)
}

fn should_request_init(is_variant_switch: bool, segment_index: usize) -> bool {
    is_variant_switch || segment_index == 0
}

/// Pure I/O executor for HLS segment fetching.
#[derive(Clone)]
pub(crate) struct HlsIo {
    fetch: Arc<DefaultFetchManager>,
}

impl HlsIo {
    pub(crate) fn new(fetch: Arc<DefaultFetchManager>) -> Self {
        Self { fetch }
    }
}

/// Plan for downloading a single HLS segment.
pub(crate) struct HlsPlan {
    pub(crate) variant: usize,
    pub(crate) segment_index: usize,
    pub(crate) need_init: bool,
    pub(crate) seek_epoch: SeekEpoch,
}

/// Result of downloading a single HLS segment.
pub(crate) struct HlsFetch {
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<Url>,
    pub(crate) media: SegmentMeta,
    pub(crate) media_cached: bool,
    pub(crate) segment_index: usize,
    pub(crate) variant: usize,
    pub(crate) duration: Duration,
    pub(crate) seek_epoch: SeekEpoch,
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
                .load_media_segment_with_source(plan.variant, plan.segment_index),
            init_fut,
        );

        let duration = start.elapsed();
        let (media, media_cached) = media_result?;

        Ok(HlsFetch {
            init_len,
            init_url,
            media,
            media_cached,
            segment_index: plan.segment_index,
            variant: plan.variant,
            duration,
            seek_epoch: plan.seek_epoch,
        })
    }
}

/// HLS downloader: fetches segments and maintains ABR state.
pub(crate) struct HlsDownloader {
    pub(crate) active_seek_epoch: SeekEpoch,
    pub(crate) io: HlsIo,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) current_segment_index: usize,
    pub(crate) gap_scan_start_segment: usize,
    pub(crate) last_committed_variant: Option<usize>,
    pub(crate) force_init_for_seek: bool,
    pub(crate) sent_init_for_variant: HashSet<usize>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) byte_offset: u64,
    pub(crate) shared: Arc<SharedSegments>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold. None = no backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
    /// Coverage index for crash-safe segment tracking.
    pub(crate) coverage: CoverageManager<StorageResource>,
}

impl HlsDownloader {
    fn classify_variant_transition(&self, variant: usize, segment_index: usize) -> (bool, bool) {
        classify_variant_transition(
            self.last_committed_variant,
            &self.sent_init_for_variant,
            variant,
            segment_index,
        )
    }

    fn reset_for_seek_epoch(
        &mut self,
        seek_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    ) {
        let previous_variant = self.last_committed_variant;
        self.active_seek_epoch = seek_epoch;
        self.shared.timeline.set_eof(false);
        self.shared
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.current_segment_index = segment_index;
        self.gap_scan_start_segment = segment_index;
        self.byte_offset = self
            .playlist_state
            .segment_byte_offset(variant, segment_index)
            .unwrap_or(0);
        self.force_init_for_seek = previous_variant
            .and_then(|index| self.playlist_state.variant_codec(index))
            .zip(self.playlist_state.variant_codec(variant))
            .is_some_and(|(from, to)| from != to);
        // Seek establishes a new baseline in the target variant timeline.
        // Treat the target as committed so non-zero seek segments do not
        // go through synthetic variant-switch init insertion.
        self.last_committed_variant = Some(variant);
        self.shared.timeline.set_total_bytes(None);
        self.shared.timeline.set_download_position(0);

        let current_variant = self.abr.get_current_variant_index();
        if current_variant != variant {
            self.abr.apply(
                &AbrDecision {
                    target_variant_index: variant,
                    reason: AbrReason::ManualOverride,
                    changed: true,
                },
                Instant::now(),
            );
        }
    }

    fn publish_download_error(&self, context: &str, error: &HlsError) {
        debug!(?error, context, "hls downloader error");
        self.bus.publish(HlsEvent::DownloadError {
            error: format!("{context}: {error}"),
        });
    }

    fn loaded_segment_offset_mismatch(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> Option<(u64, u64)> {
        let loaded_offset = {
            let segments = self.shared.segments.lock();
            segments
                .find_loaded_segment(variant, segment_index)
                .map(|segment| segment.byte_offset)?
        };
        let expected_offset = self
            .playlist_state
            .segment_byte_offset(variant, segment_index)?;
        (loaded_offset != expected_offset).then_some((loaded_offset, expected_offset))
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
        coverage: &CoverageManager<StorageResource>,
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
                .is_ok_and(|r| matches!(r.status(), ResourceStatus::Committed { .. }));
            if !init_cached {
                return (0, 0);
            }
        }

        // Get init_size from actual disk resource (size map was populated by calculate_size_map).
        #[expect(
            clippy::option_if_let_else,
            reason = "nested conditionals are clearer with if-let"
        )]
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
                let cov = coverage.open_state(segment_url.to_string());
                if cov.total_size().is_none() || !cov.is_complete() {
                    break;
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

        self.bus.publish(HlsEvent::SegmentComplete {
            variant: dl.variant,
            segment_index: dl.segment_index,
            bytes_transferred: dl.media.len,
            cached: dl.media_cached,
            duration: dl.duration,
        });

        if is_variant_switch {
            self.sent_init_for_variant.insert(dl.variant);
        }
        self.last_committed_variant = Some(dl.variant);

        let actual_init_len = if is_midstream_switch || is_variant_switch || dl.segment_index == 0 {
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
        self.shared.timeline.set_download_position(self.byte_offset);

        // Reconcile metadata: update this segment's size and recalculate subsequent
        // byte_offsets using the actual (possibly decrypted) size. For non-DRM streams
        // this is a no-op (actual == predicted). For DRM streams, it corrects the drift
        // from PKCS7 padding removal so future lookups use accurate offsets.
        let pre_total = self.playlist_state.total_variant_size(dl.variant);
        self.playlist_state
            .reconcile_segment_size(dl.variant, dl.segment_index, actual_size);
        let post_total = self.playlist_state.total_variant_size(dl.variant);

        // Update timeline total_bytes if reconciliation changed the variant total.
        // This handles cases where actual sizes differ from HEAD predictions:
        // - Larger: HTTP auto-decompression returns more bytes than Content-Length
        // - Smaller: DRM decryption removes PKCS7 padding
        // Uses delta-based adjustment so midstream switch offsets are preserved.
        if let (Some(pre), Some(post)) = (pre_total, post_total)
            && pre != post
        {
            let current = self.shared.timeline.total_bytes().unwrap_or(0);
            let new_expected = if post > pre {
                current.saturating_add(post - pre)
            } else {
                current.saturating_sub(pre - post)
            };
            self.shared.timeline.set_total_bytes(Some(new_expected));
        }

        self.bus.publish(HlsEvent::DownloadProgress {
            offset: self.byte_offset,
            total: None,
        });

        // Mark segment coverage for crash-safe tracking in coverage index.
        {
            let mut cov = self.coverage.open_state(media_url.to_string());
            cov.set_total_size(media_len);
            cov.mark(0..media_len);
        }
        {
            let mut segments = self.shared.segments.lock();
            if is_variant_switch {
                segments.fence_at(byte_offset, dl.variant);
            }
            segments.push(segment);
        }
        self.shared.condvar.notify_all();

        // Coverage flush happens via Drop, or explicitly in commit().
    }

    /// Prepare variant for download: detect switches, calculate metadata, populate cache.
    ///
    /// Returns `(is_variant_switch, is_midstream_switch)`.
    async fn ensure_variant_ready(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Result<(bool, bool), HlsError> {
        let (is_variant_switch, is_midstream_switch) =
            self.classify_variant_transition(variant, segment_index);
        self.handle_midstream_switch(is_midstream_switch);

        if self.should_prepare_variant_totals(is_variant_switch) {
            self.refresh_variant_total_bytes(
                variant,
                segment_index,
                is_variant_switch,
                is_midstream_switch,
            )
            .await;
        }

        Ok((is_variant_switch, is_midstream_switch))
    }

    fn handle_midstream_switch(&mut self, is_midstream_switch: bool) {
        if !is_midstream_switch {
            return;
        }
        self.shared
            .had_midstream_switch
            .store(true, Ordering::Release);
        while self.shared.segment_requests.pop().is_some() {}
    }

    fn should_prepare_variant_totals(&self, is_variant_switch: bool) -> bool {
        is_variant_switch || self.shared.timeline.total_bytes().unwrap_or(0) == 0
    }

    async fn refresh_variant_total_bytes(
        &mut self,
        variant: usize,
        segment_index: usize,
        is_variant_switch: bool,
        is_midstream_switch: bool,
    ) {
        if self.playlist_state.has_size_map(variant) {
            self.apply_cached_variant_total(variant);
            return;
        }

        if let Err(e) = Self::calculate_size_map(&self.playlist_state, &self.fetch, variant).await {
            debug!(?e, variant, "failed to calculate variant size map");
            return;
        }

        let total = self.playlist_state.total_variant_size(variant).unwrap_or(0);
        debug!(variant, total, "calculated and cached variant size map");

        let (cached_count, cached_end_offset) =
            self.populate_cached_segments_if_needed(variant, is_variant_switch);
        self.apply_cached_segment_progress(variant, cached_count, cached_end_offset);

        let effective_total = self.calculate_effective_total(
            variant,
            segment_index,
            total,
            cached_end_offset,
            is_midstream_switch,
        );
        if effective_total > 0 {
            self.shared.timeline.set_total_bytes(Some(effective_total));
        }
    }

    fn apply_cached_variant_total(&self, variant: usize) {
        let Some(cached_total) = self.playlist_state.total_variant_size(variant) else {
            return;
        };
        debug!(variant, total = cached_total, "using cached variant length");
        if cached_total > 0 {
            self.shared.timeline.set_total_bytes(Some(cached_total));
        }
    }

    fn populate_cached_segments_if_needed(
        &self,
        variant: usize,
        is_variant_switch: bool,
    ) -> (usize, u64) {
        if is_variant_switch {
            return (0, 0);
        }
        Self::populate_cached_segments(&self.shared, &self.fetch, variant, &self.coverage)
    }

    fn apply_cached_segment_progress(
        &mut self,
        variant: usize,
        cached_count: usize,
        cached_end_offset: u64,
    ) {
        if cached_count == 0 {
            return;
        }

        if cached_end_offset > self.byte_offset {
            self.byte_offset = cached_end_offset;
        }
        self.shared.timeline.set_download_position(self.byte_offset);
        if cached_count > self.current_segment_index {
            self.current_segment_index = cached_count;
        }
        self.sent_init_for_variant.insert(variant);
    }

    fn calculate_effective_total(
        &self,
        variant: usize,
        segment_index: usize,
        total: u64,
        cached_end_offset: u64,
        is_midstream_switch: bool,
    ) -> u64 {
        if !is_midstream_switch {
            let result = total.max(cached_end_offset);
            debug!(
                variant,
                metadata_total = total,
                cached_end_offset,
                effective_total = result,
                "effective_total: normal (no switch)"
            );
            return result;
        }

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
    }

    async fn poll_demand_impl(&mut self) -> Option<HlsPlan> {
        if self.shared.timeline.is_flushing() {
            yield_now().await;
            return None;
        }

        let req = self.next_valid_demand_request()?;
        debug!(
            variant = req.variant,
            segment_index = req.segment_index,
            "processing on-demand segment request"
        );

        let (req, num_segments) = self.num_segments_for_demand(req).await?;
        if Self::demand_request_out_of_range(&req, num_segments) {
            self.shared.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded at stale offset, refreshing demand request",
            "segment already loaded, skipping",
        ) {
            self.shared.condvar.notify_all();
            return None;
        }

        let (is_variant_switch, is_midstream_switch) = self
            .prepare_variant_for_demand(req.variant, req.segment_index)
            .await?;

        if self.should_skip_pre_switch_variant(req.variant, req.segment_index, is_midstream_switch)
        {
            self.shared.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded with stale offset after metadata calc, refreshing",
            "segment loaded from cache after metadata calc",
        ) {
            self.shared.condvar.notify_all();
            return None;
        }

        Some(self.build_demand_plan(&req, is_variant_switch))
    }

    fn next_valid_demand_request(&mut self) -> Option<SegmentRequest> {
        loop {
            let req = self.shared.segment_requests.pop()?;
            let current_epoch = self.shared.timeline.seek_epoch();
            if req.seek_epoch == current_epoch {
                if req.seek_epoch != self.active_seek_epoch {
                    self.reset_for_seek_epoch(req.seek_epoch, req.variant, req.segment_index);
                }
                return Some(req);
            }

            debug!(
                req_epoch = req.seek_epoch,
                current_epoch,
                variant = req.variant,
                segment_index = req.segment_index,
                "dropping stale on-demand request"
            );
            self.bus.publish(HlsEvent::StaleRequestDropped {
                seek_epoch: req.seek_epoch,
                current_epoch,
                variant: req.variant,
                segment_index: req.segment_index,
            });
            self.shared.condvar.notify_all();
        }
    }

    async fn num_segments_for_demand(
        &mut self,
        req: SegmentRequest,
    ) -> Option<(SegmentRequest, usize)> {
        match self.fetch.num_segments(req.variant).await {
            Ok(value) => Some((req, value)),
            Err(e) => {
                self.publish_download_error("failed to query segment count for demand", &e);
                self.shared.segment_requests.push(req);
                self.shared.condvar.notify_all();
                None
            }
        }
    }

    fn demand_request_out_of_range(req: &SegmentRequest, num_segments: usize) -> bool {
        if req.segment_index < num_segments {
            return false;
        }
        debug!(
            variant = req.variant,
            segment_index = req.segment_index,
            num_segments,
            "segment index out of range"
        );
        true
    }

    fn segment_loaded_for_demand(
        &self,
        variant: usize,
        segment_index: usize,
        stale_reason: &str,
        loaded_reason: &str,
    ) -> bool {
        if let Some((loaded_offset, expected_offset)) =
            self.loaded_segment_offset_mismatch(variant, segment_index)
        {
            debug!(
                variant,
                segment_index,
                loaded_offset,
                expected_offset,
                reason = stale_reason,
                "demand segment has stale offset"
            );
            return false;
        }

        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(variant, segment_index)
        {
            debug!(
                variant,
                segment_index,
                reason = loaded_reason,
                "demand segment already loaded"
            );
            return true;
        }
        false
    }

    async fn prepare_variant_for_demand(
        &mut self,
        variant: usize,
        segment_index: usize,
    ) -> Option<(bool, bool)> {
        match self.ensure_variant_ready(variant, segment_index).await {
            Ok(flags) => Some(flags),
            Err(e) => {
                self.publish_download_error("variant preparation error in poll_demand", &e);
                self.shared.condvar.notify_all();
                None
            }
        }
    }

    fn should_skip_pre_switch_variant(
        &self,
        variant: usize,
        segment_index: usize,
        is_midstream_switch: bool,
    ) -> bool {
        if !self.shared.had_midstream_switch.load(Ordering::Acquire) || is_midstream_switch {
            return false;
        }
        let current_variant = self.abr.get_current_variant_index();
        if variant == current_variant {
            return false;
        }
        debug!(
            variant,
            segment_index, current_variant, "skipping stale segment from pre-switch variant"
        );
        true
    }

    fn build_demand_plan(&mut self, req: &SegmentRequest, is_variant_switch: bool) -> HlsPlan {
        self.bus.publish(HlsEvent::SegmentStart {
            variant: req.variant,
            segment_index: req.segment_index,
            byte_offset: self.byte_offset,
        });

        let need_init =
            self.force_init_for_seek || should_request_init(is_variant_switch, req.segment_index);
        if need_init {
            self.force_init_for_seek = false;
        }

        HlsPlan {
            variant: req.variant,
            segment_index: req.segment_index,
            need_init,
            seek_epoch: req.seek_epoch,
        }
    }

    async fn plan_impl(&mut self) -> PlanOutcome<HlsPlan> {
        if self.shared.timeline.is_flushing() {
            yield_now().await;
            return PlanOutcome::Batch(Vec::new());
        }

        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let variant = self.abr.get_current_variant_index();

        let Some(num_segments) = self.num_segments_for_plan(variant).await else {
            return PlanOutcome::Batch(Vec::new());
        };

        if self.handle_tail_state(variant, num_segments).await {
            return PlanOutcome::Batch(Vec::new());
        }

        self.publish_variant_applied(old_variant, variant, &decision);

        let (is_variant_switch, is_midstream_switch) = match self
            .ensure_variant_ready(variant, self.current_segment_index)
            .await
        {
            Ok(flags) => flags,
            Err(e) => {
                self.shared.condvar.notify_all();
                self.publish_download_error("variant preparation error", &e);
                return PlanOutcome::Complete;
            }
        };

        let (plans, batch_end) = self.build_batch_plans(
            variant,
            num_segments,
            is_variant_switch,
            is_midstream_switch,
        );

        if plans.is_empty() {
            self.current_segment_index = batch_end;
            self.shared.condvar.notify_all();
        }

        PlanOutcome::Batch(plans)
    }

    async fn num_segments_for_plan(&mut self, variant: usize) -> Option<usize> {
        match self.fetch.num_segments(variant).await {
            Ok(value) => Some(value),
            Err(e) => {
                self.publish_download_error("failed to query segment count", &e);
                self.shared.condvar.notify_all();
                yield_now().await;
                None
            }
        }
    }

    async fn handle_tail_state(&mut self, variant: usize, num_segments: usize) -> bool {
        if self.current_segment_index < num_segments {
            return false;
        }

        let timeline_seek_epoch = self.shared.timeline.seek_epoch();
        if timeline_seek_epoch != self.active_seek_epoch {
            self.shared.timeline.set_eof(false);
            self.shared.condvar.notify_all();
            yield_now().await;
            return true;
        }

        let had_midstream_switch = self.shared.had_midstream_switch.load(Ordering::Acquire);
        if !had_midstream_switch {
            if self.rewind_to_first_missing_segment(variant, num_segments) {
                return true;
            }
        } else {
            debug!(
                variant,
                num_segments,
                "playlist tail reached after midstream switch; skip automatic backfill"
            );
        }

        if !self.shared.timeline.eof() {
            debug!("reached end of playlist");
            self.shared.timeline.set_eof(true);
            self.bus.publish(HlsEvent::EndOfStream);
        }
        self.shared.condvar.notify_all();
        true
    }

    fn rewind_to_first_missing_segment(&mut self, variant: usize, num_segments: usize) -> bool {
        let missing_segment = {
            let segments = self.shared.segments.lock();
            first_missing_segment(
                &segments,
                variant,
                self.gap_scan_start_segment,
                num_segments,
            )
        };
        let Some(segment_index) = missing_segment else {
            return false;
        };

        debug!(
            variant,
            segment_index,
            num_segments,
            "playlist tail reached with gaps; rewinding to first missing segment"
        );
        self.current_segment_index = segment_index;
        self.shared.condvar.notify_all();
        true
    }

    fn publish_variant_applied(&self, old_variant: usize, variant: usize, decision: &AbrDecision) {
        if !decision.changed {
            return;
        }
        self.bus.publish(HlsEvent::VariantApplied {
            from_variant: old_variant,
            to_variant: variant,
            reason: decision.reason,
        });
    }

    fn build_batch_plans(
        &mut self,
        variant: usize,
        num_segments: usize,
        is_variant_switch: bool,
        is_midstream_switch: bool,
    ) -> (Vec<HlsPlan>, usize) {
        let batch_end = (self.current_segment_index + self.prefetch_count).min(num_segments);
        let seek_epoch = self.shared.timeline.seek_epoch();
        let mut plans = Vec::new();
        let mut need_init = self.force_init_for_seek || is_variant_switch;

        for seg_idx in self.current_segment_index..batch_end {
            if self.should_skip_planned_segment(variant, seg_idx, is_midstream_switch) {
                continue;
            }

            self.bus.publish(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: self.byte_offset,
            });

            let plan_need_init = should_request_init(need_init, seg_idx);
            plans.push(HlsPlan {
                variant,
                segment_index: seg_idx,
                need_init: plan_need_init,
                seek_epoch,
            });
            if plan_need_init {
                self.force_init_for_seek = false;
            }
            need_init = false;
        }

        (plans, batch_end)
    }

    fn should_skip_planned_segment(
        &mut self,
        variant: usize,
        seg_idx: usize,
        is_midstream_switch: bool,
    ) -> bool {
        if let Some((loaded_offset, expected_offset)) =
            self.loaded_segment_offset_mismatch(variant, seg_idx)
        {
            debug!(
                variant,
                segment_index = seg_idx,
                loaded_offset,
                expected_offset,
                "segment in plan window has stale offset, forcing refresh"
            );
            return false;
        }

        if self
            .shared
            .segments
            .lock()
            .is_segment_loaded(variant, seg_idx)
        {
            self.current_segment_index = seg_idx + 1;
            return true;
        }

        let had_switch = self.shared.had_midstream_switch.load(Ordering::Acquire);
        if !had_switch || is_midstream_switch {
            return false;
        }

        let current = self.abr.get_current_variant_index();
        variant != current
    }

    fn make_abr_decision(&mut self) -> AbrDecision {
        let now = Instant::now();
        let current_variant = self.abr.get_current_variant_index();
        let decision = self.abr.decide(now);

        if decision.changed {
            let cross_codec = is_cross_codec_switch(
                &self.playlist_state,
                current_variant,
                decision.target_variant_index,
            );
            debug!(
                from = current_variant,
                to = decision.target_variant_index,
                cross_codec,
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

        #[expect(
            clippy::cast_precision_loss,
            reason = "throughput estimation tolerates f64 precision"
        )]
        let bytes_per_second = if duration.as_secs_f64() > 0.0 {
            bytes as f64 / duration.as_secs_f64()
        } else {
            0.0
        };
        self.bus
            .publish(HlsEvent::ThroughputSample { bytes_per_second });
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
        self.poll_demand_impl().await
    }

    async fn plan(&mut self) -> PlanOutcome<HlsPlan> {
        self.plan_impl().await
    }

    fn commit(&mut self, fetch: HlsFetch) {
        let current_epoch = self.shared.timeline.seek_epoch();
        if is_stale_epoch(fetch.seek_epoch, current_epoch) {
            debug!(
                fetch_epoch = fetch.seek_epoch,
                current_epoch,
                variant = fetch.variant,
                segment_index = fetch.segment_index,
                "dropping stale fetch before commit"
            );
            self.bus.publish(HlsEvent::StaleFetchDropped {
                seek_epoch: fetch.seek_epoch,
                current_epoch,
                variant: fetch.variant,
                segment_index: fetch.segment_index,
            });
            self.shared.condvar.notify_all();
            return;
        }

        let (is_variant_switch, is_midstream_switch) =
            self.classify_variant_transition(fetch.variant, fetch.segment_index);

        if fetch.init_len > 0 {
            self.sent_init_for_variant.insert(fetch.variant);
        }

        // Only advance sequential position for the next expected segment.
        // On-demand loads and out-of-order batch results must not jump past gaps --
        // plan() handles skipping loaded segments when building the next batch.
        if fetch.segment_index == self.current_segment_index {
            self.current_segment_index = fetch.segment_index + 1;
        }

        self.commit_segment(fetch, is_variant_switch, is_midstream_switch);
    }

    fn should_throttle(&self) -> bool {
        // Never throttle during seek — downloader must be free to respond.
        if self.shared.timeline.is_flushing() {
            return false;
        }
        let Some(limit) = self.look_ahead_bytes else {
            return false;
        };

        let reader_pos = self.shared.timeline.byte_position();
        let downloaded = self.shared.segments.lock().max_end_offset();

        downloaded.saturating_sub(reader_pos) > limit
    }

    async fn wait_ready(&self) {
        if self.shared.timeline.is_flushing() || !self.should_throttle() {
            return;
        }
        self.shared.reader_advanced.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc, time::Duration};

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
    use kithara_coverage::{Coverage, CoverageManager};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
    use kithara_stream::{AudioCodec, Downloader, PlanOutcome, Timeline};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        DownloadState, HlsDownloader, LoadedSegment, classify_variant_transition,
        first_missing_segment, is_cross_codec_switch, is_stale_epoch, should_request_init,
    };
    use crate::{
        config::HlsConfig,
        fetch::{DefaultFetchManager, FetchManager},
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistAccess, PlaylistState, SegmentState, VariantSizeMap, VariantState},
        source::{SharedSegments, build_pair},
    };

    #[kithara::test]
    fn commit_drops_stale_fetch_epoch() {
        assert!(is_stale_epoch(7, 8));
        assert!(!is_stale_epoch(9, 9));
    }

    #[kithara::test]
    fn first_missing_segment_detects_gap() {
        let mut state = DownloadState::new();
        let media_url = Url::parse("https://example.com/seg.m4s").expect("valid URL");
        state.push(LoadedSegment {
            variant: 0,
            segment_index: 0,
            byte_offset: 0,
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url: media_url.clone(),
        });
        state.push(LoadedSegment {
            variant: 0,
            segment_index: 2,
            byte_offset: 200,
            init_len: 0,
            media_len: 100,
            init_url: None,
            media_url,
        });

        assert_eq!(first_missing_segment(&state, 0, 0, 3), Some(1));
        assert_eq!(first_missing_segment(&state, 0, 2, 3), None);
        assert_eq!(first_missing_segment(&state, 0, 0, 1), None);
    }

    fn make_variant_state_with_segments(
        id: usize,
        codec: Option<AudioCodec>,
        segment_count: usize,
    ) -> VariantState {
        let base = Url::parse("https://example.com/").expect("valid base URL");
        VariantState {
            id,
            uri: base
                .join(&format!("v{id}.m3u8"))
                .expect("valid playlist URL"),
            bandwidth: Some(128_000),
            codec,
            container: None,
            init_url: None,
            segments: (0..segment_count)
                .map(|index| SegmentState {
                    index,
                    url: base
                        .join(&format!("seg-{id}-{index}.m4s"))
                        .expect("valid segment URL"),
                    duration: Duration::from_secs(4),
                    key: None,
                })
                .collect(),
            size_map: None,
        }
    }

    fn make_variant_state(id: usize, codec: Option<AudioCodec>) -> VariantState {
        make_variant_state_with_segments(id, codec, 1)
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

    fn make_coverage_manager() -> CoverageManager<StorageResource> {
        let backend = AssetStoreBuilder::new()
            .ephemeral(true)
            .cancel(CancellationToken::new())
            .build();
        backend
            .open_coverage_manager()
            .expect("coverage manager should open")
    }

    #[kithara::test]
    fn cross_codec_switch_detects_incompatible_variants() {
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, Some(AudioCodec::AacLc)),
            make_variant_state(1, Some(AudioCodec::Flac)),
        ]));
        assert!(is_cross_codec_switch(&playlist_state, 0, 1));
    }

    #[kithara::test]
    fn cross_codec_switch_allows_same_codec_variants() {
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, Some(AudioCodec::AacLc)),
            make_variant_state(1, Some(AudioCodec::AacLc)),
        ]));
        assert!(!is_cross_codec_switch(&playlist_state, 0, 1));
    }

    #[kithara::test]
    fn classify_same_variant_seek_is_not_midstream_switch() {
        let mut sent_init_for_variant = HashSet::new();
        let variant = 1;
        let segment_index = 37;
        let last_committed_variant = Some(variant);

        let (is_variant_switch, is_midstream_switch) = classify_variant_transition(
            last_committed_variant,
            &sent_init_for_variant,
            variant,
            segment_index,
        );

        assert!(
            !is_variant_switch,
            "seek within same variant must not trigger variant-switch init path"
        );
        assert!(
            !is_midstream_switch,
            "seek within same variant must not trigger midstream switch path"
        );

        sent_init_for_variant.insert(variant);
        let (is_variant_switch, is_midstream_switch) = classify_variant_transition(
            last_committed_variant,
            &sent_init_for_variant,
            variant,
            segment_index,
        );
        assert!(!is_variant_switch);
        assert!(!is_midstream_switch);
    }

    #[kithara::test]
    fn classify_real_variant_change_marks_midstream_switch_only_after_segment_zero() {
        let sent_init_for_variant = HashSet::new();
        let from_variant = Some(0);
        let to_variant = 1;

        let (is_variant_switch, is_midstream_switch) =
            classify_variant_transition(from_variant, &sent_init_for_variant, to_variant, 0);
        assert!(is_variant_switch);
        assert!(!is_midstream_switch);

        let (is_variant_switch, is_midstream_switch) =
            classify_variant_transition(from_variant, &sent_init_for_variant, to_variant, 5);
        assert!(is_variant_switch);
        assert!(is_midstream_switch);
    }

    #[kithara::test(tokio)]
    async fn plan_while_flushing_does_not_mark_eof() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
            0,
            Some(AudioCodec::AacLc),
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.current_segment_index = 1; // tail
        downloader.shared.timeline.set_eof(false);
        let _ = downloader
            .shared
            .timeline
            .initiate_seek(Duration::from_secs(1));
        assert!(downloader.shared.timeline.is_flushing());

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Batch(_)));
        assert!(
            !downloader.shared.timeline.eof(),
            "plan must not set EOF while seek flushing is active"
        );
    }

    #[kithara::test(tokio)]
    async fn plan_with_new_seek_epoch_does_not_mark_eof_from_stale_tail() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
            0,
            Some(AudioCodec::AacLc),
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.current_segment_index = 1; // tail
        downloader.shared.timeline.set_eof(false);

        let epoch = downloader
            .shared
            .timeline
            .initiate_seek(Duration::from_secs(1));
        downloader.shared.timeline.complete_seek(epoch);
        assert!(!downloader.shared.timeline.is_flushing());
        assert_ne!(
            downloader.shared.timeline.seek_epoch(),
            downloader.active_seek_epoch
        );

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Batch(_)));
        assert!(
            !downloader.shared.timeline.eof(),
            "plan must not emit EOF while seek epoch is newer than downloader state"
        );
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_keeps_init_markers_on_known_variant() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state(
            0,
            Some(AudioCodec::AacLc),
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.sent_init_for_variant.insert(0);

        downloader.reset_for_seek_epoch(1, 0, 0);

        assert!(
            downloader.sent_init_for_variant.contains(&0),
            "known variant should keep init marker after seek reset"
        );
        assert!(
            !downloader.force_init_for_seek,
            "same-codec seek reset should not force init"
        );
        assert_eq!(downloader.last_committed_variant, Some(0));
        assert_eq!(downloader.gap_scan_start_segment, 0);
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_to_unseen_variant_sets_new_baseline() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, Some(AudioCodec::AacLc)),
            make_variant_state(1, Some(AudioCodec::AacLc)),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.sent_init_for_variant.insert(0);

        downloader.reset_for_seek_epoch(2, 1, 0);

        assert!(
            downloader.sent_init_for_variant.contains(&0),
            "known variant markers must be preserved on reset"
        );
        assert!(
            !downloader.sent_init_for_variant.contains(&1),
            "unseen variant must remain without init marker"
        );
        assert!(
            !downloader.force_init_for_seek,
            "same-codec seek to unseen variant should not force init"
        );
        assert_eq!(downloader.last_committed_variant, Some(1));
        assert_eq!(downloader.gap_scan_start_segment, 0);
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_forces_init_on_cross_codec_seek() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state(0, Some(AudioCodec::AacLc)),
            make_variant_state(1, Some(AudioCodec::Flac)),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.reset_for_seek_epoch(3, 1, 2);

        assert!(
            downloader.force_init_for_seek,
            "cross-codec seek reset must force init for the first segment"
        );
        assert_eq!(downloader.last_committed_variant, Some(1));
        assert_eq!(downloader.gap_scan_start_segment, 2);
    }

    #[kithara::test]
    fn should_request_init_for_segment_zero_even_when_variant_is_known() {
        assert!(should_request_init(false, 0));
        assert!(!should_request_init(false, 1));
    }

    #[kithara::test]
    fn should_request_init_only_for_segment_zero_or_variant_switch() {
        assert!(!should_request_init(false, 5));
        assert!(should_request_init(true, 5));
    }

    #[kithara::test(tokio)]
    async fn loaded_segment_offset_mismatch_detects_shifted_loaded_segment() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            2,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100],
                segment_sizes: vec![100, 100],
                total: 200,
            },
        );

        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let coverage = make_coverage_manager();
        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            coverage,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        {
            let mut segments = downloader.shared.segments.lock();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 120,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid URL"),
            });
        }

        assert_eq!(
            downloader.loaded_segment_offset_mismatch(0, 1),
            Some((120, 100))
        );
    }

    #[kithara::test(native)]
    fn populate_cached_segments_requires_coverage_metadata() {
        let cancel = CancellationToken::new();
        let temp_dir = TempDir::new().expect("temp dir");
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            2,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100],
                segment_sizes: vec![100, 100],
                total: 200,
            },
        );

        let noop_drm: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });
        let backend = AssetStoreBuilder::new()
            .root_dir(temp_dir.path())
            .asset_root(Some("populate-cached-segments"))
            .cancel(cancel.clone())
            .process_fn(noop_drm)
            .build();
        let net = HttpClient::new(NetOptions::default());
        let fetch = Arc::new(FetchManager::new(backend, net, cancel.clone()));
        let shared = Arc::new(SharedSegments::new(
            cancel,
            Arc::clone(&playlist_state),
            Timeline::new(),
        ));
        let coverage = fetch
            .backend()
            .open_coverage_manager()
            .expect("coverage manager should open");

        let segment_url = playlist_state.segment_url(0, 0).expect("segment URL");
        let key = ResourceKey::from_url(&segment_url);
        let resource = fetch
            .backend()
            .open_resource(&key)
            .expect("segment resource should open");
        resource
            .write_at(0, &[0xAB; 100])
            .expect("write segment bytes");
        resource.commit(Some(100)).expect("commit segment bytes");

        let (count_without_coverage, end_without_coverage) =
            HlsDownloader::populate_cached_segments(&shared, &fetch, 0, &coverage);
        assert_eq!(count_without_coverage, 0);
        assert_eq!(end_without_coverage, 0);

        {
            let mut state = coverage.open_state(segment_url.to_string());
            state.set_total_size(100);
            state.mark(0..100);
        }

        assert!(matches!(
            resource.status(),
            ResourceStatus::Committed {
                final_len: Some(100)
            }
        ));
        let reopened = fetch
            .backend()
            .open_resource(&key)
            .expect("segment resource should reopen");
        assert!(matches!(
            reopened.status(),
            ResourceStatus::Committed {
                final_len: Some(100)
            }
        ));
        let state = coverage.open_state(segment_url.to_string());
        assert_eq!(state.total_size(), Some(100));
        assert!(state.is_complete());

        let (count_with_coverage, end_with_coverage) =
            HlsDownloader::populate_cached_segments(&shared, &fetch, 0, &coverage);
        assert_eq!(count_with_coverage, 1);
        assert_eq!(end_with_coverage, 100);
    }
}
