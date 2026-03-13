//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use kithara_abr::{
    AbrController, AbrDecision, AbrReason, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource,
};
use kithara_assets::{AssetResourceState, ResourceKey};
use kithara_events::{EventBus, HlsEvent, SeekEpoch};
use kithara_platform::{time::Instant, tokio};
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
use kithara_stream::{DownloadCursor, Downloader, DownloaderIo, PlanOutcome};
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    download_state::{DownloadProgress, DownloadState, LoadedSegment},
    fetch::{DefaultFetchManager, Loader, SegmentMeta},
    layout::{
        LayoutAnchor, expected_layout_offset as resolve_expected_layout_offset,
        inferred_layout_anchor as resolve_inferred_layout_anchor,
    },
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
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
            self.fetch.load_media_segment_with_source_for_epoch(
                plan.variant,
                plan.segment_index,
                plan.seek_epoch,
            ),
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
    pub(crate) cursor: DownloadCursor<usize>,
    pub(crate) last_committed_variant: Option<usize>,
    pub(crate) force_init_for_seek: bool,
    pub(crate) sent_init_for_variant: HashSet<usize>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) segments: Arc<kithara_platform::Mutex<DownloadState>>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold (bytes). None = no byte-based backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Backpressure threshold (segments ahead of reader). None = no limit.
    /// For ephemeral backends, derived from cache capacity to prevent
    /// evicting segments the reader still needs.
    pub(crate) look_ahead_segments: Option<usize>,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
}

impl HlsDownloader {
    fn current_segment_index(&self) -> usize {
        self.cursor.fill_next().unwrap_or(0)
    }

    fn num_segments(&self, variant: usize) -> Option<usize> {
        self.playlist_state.num_segments(variant)
    }

    fn gap_scan_start_segment(&self) -> usize {
        self.cursor.fill_floor().unwrap_or(0)
    }

    fn reset_cursor(&mut self, segment_index: usize) {
        self.cursor.reset_fill(segment_index);
    }

    fn advance_current_segment_index(&mut self, segment_index: usize) {
        self.cursor.advance_fill_to(segment_index);
    }

    fn rewind_current_segment_index(&mut self, segment_index: usize) {
        self.cursor.rewind_fill_to(segment_index);
    }

    fn is_below_switch_floor(&self, variant: usize, segment_index: usize) -> bool {
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) {
            return false;
        }
        let current_variant = self.abr.get_current_variant_index();
        variant == current_variant && segment_index < self.gap_scan_start_segment()
    }

    fn switched_layout_anchor(&self) -> Option<(usize, u64)> {
        let variant = self.last_committed_variant?;
        let (segment_index, byte_offset) = {
            let segments = self.segments.lock_sync();
            let anchor = segments.first_segment_of_variant(variant)?;
            let tuple = (anchor.segment_index, anchor.byte_offset);
            drop(segments);
            tuple
        };
        ((segment_index > 0) || (byte_offset > 0)).then_some((variant, byte_offset))
    }

    fn is_stale_cross_codec_fetch(&self, fetch: &HlsFetch) -> bool {
        let Some((current_variant, anchor_offset)) = self.switched_layout_anchor() else {
            return false;
        };
        if fetch.variant == current_variant
            || !is_cross_codec_switch(&self.playlist_state, current_variant, fetch.variant)
        {
            return false;
        }

        let fetch_offset = self
            .playlist_state
            .segment_byte_offset(fetch.variant, fetch.segment_index)
            .unwrap_or_else(|| self.coord.timeline().download_position());

        fetch_offset >= anchor_offset
    }

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
        let is_same_variant = previous_variant.is_some_and(|prev| prev == variant);

        self.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);

        self.force_init_for_seek = previous_variant
            .and_then(|index| self.playlist_state.variant_codec(index))
            .zip(self.playlist_state.variant_codec(variant))
            .is_some_and(|(from, to)| from != to);

        // Seek establishes a new baseline in the target variant timeline.
        // Treat the target as committed so non-zero seek segments do not
        // go through synthetic variant-switch init insertion.
        self.last_committed_variant = Some(variant);

        // Same-variant seek must preserve the current effective layout total.
        // After a midstream switch the stream no longer spans the full variant
        // byte space, so replacing it with metadata_total breaks exact EOF
        // detection on revisit seeks.
        let current_total = self.coord.timeline().total_bytes();
        let variant_total = self.playlist_state.total_variant_size(variant);
        let next_total = if is_same_variant {
            current_total.or(variant_total)
        } else {
            variant_total.or(current_total)
        };
        self.coord.timeline().set_total_bytes(next_total);

        if is_same_variant {
            // Same variant: keep download_position at committed watermark.
            // Segments are still in DownloadState — downloader will check
            // segment_loaded_for_demand() and skip already-loaded segments.
            let watermark = self.coord.timeline().download_position();
            let target_offset = self
                .playlist_state
                .segment_byte_offset(variant, segment_index)
                .unwrap_or(0);
            self.coord
                .timeline()
                .set_download_position(watermark.max(target_offset));
        } else {
            // Different variant: full reset from metadata offset.
            let download_position = self
                .playlist_state
                .segment_byte_offset(variant, segment_index)
                .unwrap_or(0);
            self.coord
                .timeline()
                .set_download_position(download_position);
        }

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

    fn switch_needs_init(&self, variant: usize, is_variant_switch: bool) -> bool {
        if !is_variant_switch {
            return false;
        }

        self.last_committed_variant
            .is_some_and(|previous| is_cross_codec_switch(&self.playlist_state, previous, variant))
    }

    fn variant_has_init(&self, variant: usize) -> bool {
        self.playlist_state.init_url(variant).is_some()
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
        let contiguous_layout = {
            let segments = self.segments.lock_sync();
            let segment = segments.find_loaded_segment(variant, segment_index)?;
            let prev_contiguous = segment_index
                .checked_sub(1)
                .and_then(|idx| segments.find_loaded_segment(variant, idx))
                .is_some_and(|prev| prev.end_offset() == segment.byte_offset);
            let next_contiguous = segments
                .find_loaded_segment(variant, segment_index + 1)
                .is_some_and(|next| segment.end_offset() == next.byte_offset);
            if prev_contiguous || next_contiguous {
                return None;
            }
            let byte_offset = segment.byte_offset;
            drop(segments);
            byte_offset
        };
        let expected_offset = self.expected_layout_offset(variant, segment_index)?;
        (contiguous_layout != expected_offset).then_some((contiguous_layout, expected_offset))
    }

    fn loaded_segment_offset(&self, variant: usize, segment_index: usize) -> Option<u64> {
        let segments = self.segments.lock_sync();
        segments
            .find_loaded_segment(variant, segment_index)
            .map(|segment| segment.byte_offset)
    }

    fn expected_layout_offset(&self, variant: usize, segment_index: usize) -> Option<u64> {
        let allow_infer = self.last_committed_variant == Some(variant);
        let had_midstream_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
        let segments = self.segments.lock_sync();
        resolve_expected_layout_offset(
            &self.playlist_state,
            &segments,
            variant,
            segment_index,
            allow_infer,
            had_midstream_switch,
        )
    }

    fn inferred_layout_anchor(&self, variant: usize) -> Option<LayoutAnchor> {
        let allow_infer = self.last_committed_variant == Some(variant);
        let had_midstream_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
        let segments = self.segments.lock_sync();
        resolve_inferred_layout_anchor(
            &self.playlist_state,
            &segments,
            variant,
            allow_infer,
            had_midstream_switch,
        )
    }

    fn inferred_layout_base(&self, variant: usize) -> Option<u64> {
        self.inferred_layout_anchor(variant)
            .map(|anchor| anchor.loaded_offset)
    }

    fn reader_segment_hint(&self, variant: usize) -> usize {
        let reader_offset = self.coord.timeline().byte_position();
        self.playlist_state
            .find_segment_at_offset(variant, reader_offset)
            .unwrap_or_else(|| self.current_segment_index())
    }

    /// Check whether a segment's init+media resources are still available.
    ///
    /// For disk backends this always returns `true` (files survive LRU eviction).
    /// For ephemeral backends, verifies LRU presence via `has_resource` (peek).
    fn segment_resources_available(&self, seg: &LoadedSegment) -> bool {
        let backend = self.fetch.backend();
        if !backend.is_ephemeral() {
            return true;
        }
        if seg.init_len > 0
            && let Some(ref init_url) = seg.init_url
            && !matches!(
                backend.resource_state(&ResourceKey::from_url(init_url)),
                Ok(AssetResourceState::Committed { .. })
            )
        {
            return false;
        }
        matches!(
            backend.resource_state(&ResourceKey::from_url(&seg.media_url)),
            Ok(AssetResourceState::Committed { .. })
        )
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
        segments: &kithara_platform::Mutex<DownloadState>,
        coord: &HlsCoord,
        fetch: &DefaultFetchManager,
        playlist_state: &PlaylistState,
        variant: usize,
    ) -> (usize, u64) {
        // Ephemeral backend has no persistent cache to scan.
        if fetch.backend().is_ephemeral() {
            return (0, 0);
        }

        let backend = fetch.backend();

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

        let mut segments = segments.lock_sync();
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
            coord.condvar.notify_all();
        }

        (count, cumulative_offset)
    }

    /// Check if this (variant, `segment_index`) is already committed at the correct offset.
    ///
    /// We check `byte_offset` because after a seek + variant switch, the same
    /// (variant, `segment_index`) may be loaded at a different cumulative offset —
    /// skipping that commit would create a hole visible to the reader.
    fn is_duplicate_commit(&self, dl: &HlsFetch) -> bool {
        let existing_offset = self
            .segments
            .lock_sync()
            .find_loaded_segment(dl.variant, dl.segment_index)
            .map(|s| s.byte_offset);

        if let Some(byte_offset) = existing_offset {
            let expected_offset = self
                .playlist_state
                .segment_byte_offset(dl.variant, dl.segment_index)
                .unwrap_or(byte_offset);
            if byte_offset == expected_offset {
                debug!(
                    variant = dl.variant,
                    segment_index = dl.segment_index,
                    byte_offset,
                    "commit_segment: skipping duplicate at correct offset"
                );
                return true;
            }
        }
        false
    }

    /// Choose `byte_offset` for segment placement.
    ///
    /// A fresh midstream switch appends at the current download cursor.
    /// Once the shifted layout is established, both sequential loads and
    /// revisits must resolve to the segment's inferred shifted offset.
    fn resolve_byte_offset(&self, dl: &HlsFetch, is_midstream_switch: bool) -> u64 {
        if let Some(loaded_offset) = self.loaded_segment_offset(dl.variant, dl.segment_index) {
            return loaded_offset;
        }

        let current_download = self.coord.timeline().download_position();
        if is_midstream_switch {
            current_download
        } else if self.inferred_layout_base(dl.variant).is_some() {
            self.expected_layout_offset(dl.variant, dl.segment_index)
                .unwrap_or(current_download)
        } else {
            self.playlist_state
                .segment_byte_offset(dl.variant, dl.segment_index)
                .unwrap_or(current_download)
        }
    }

    /// Reconcile metadata sizes and adjust timeline `total_bytes` if needed.
    ///
    /// Updates this segment's size in playlist metadata and recalculates
    /// subsequent `byte_offsets` using the actual (possibly decrypted) size.
    /// For non-DRM streams this is a no-op (actual == predicted). For DRM
    /// streams, it corrects the drift from PKCS7 padding removal.
    fn reconcile_total_bytes(&self, variant: usize, segment_index: usize, actual_size: u64) {
        let pre_total = self.playlist_state.total_variant_size(variant);
        self.playlist_state
            .reconcile_segment_size(variant, segment_index, actual_size);
        let post_total = self.playlist_state.total_variant_size(variant);

        if let (Some(pre), Some(post)) = (pre_total, post_total)
            && pre != post
        {
            let current = self.coord.timeline().total_bytes().unwrap_or(0);
            let new_expected = if post > pre {
                current.saturating_add(post - pre)
            } else {
                current.saturating_sub(pre - post)
            };
            self.coord.timeline().set_total_bytes(Some(new_expected));
        }
    }

    /// Commit a downloaded segment to the segment index.
    ///
    /// `is_variant_switch` / `is_midstream_switch` control init segment inclusion.
    fn commit_segment(&mut self, dl: HlsFetch, is_variant_switch: bool, is_midstream_switch: bool) {
        if self.is_duplicate_commit(&dl) {
            return;
        }

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

        let byte_offset = self.resolve_byte_offset(&dl, is_midstream_switch);
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
        let current_download = self.coord.timeline().download_position();
        let next_download = current_download.max(end);
        self.coord.timeline().set_download_position(next_download);

        self.reconcile_total_bytes(dl.variant, dl.segment_index, actual_size);

        self.bus.publish(HlsEvent::DownloadProgress {
            offset: next_download,
            total: None,
        });

        {
            let mut segments = self.segments.lock_sync();
            if is_variant_switch {
                segments.fence_at(byte_offset, dl.variant);
            }
            segments.push(segment);
        }
        self.coord.condvar.notify_all();
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
        self.cursor
            .reopen_fill(self.current_segment_index(), self.current_segment_index());
        self.coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        self.coord.clear_segment_requests();
        // Wake blocked wait_range so it can re-push its on-demand request.
        // The `had_midstream_switch` flag tells wait_range to clear
        // `on_demand_pending`, allowing the re-push for the new variant.
        self.coord.condvar.notify_all();
    }

    fn should_prepare_variant_totals(&self, is_variant_switch: bool) -> bool {
        is_variant_switch || self.coord.timeline().total_bytes().unwrap_or(0) == 0
    }

    async fn refresh_variant_total_bytes(
        &mut self,
        variant: usize,
        segment_index: usize,
        is_variant_switch: bool,
        is_midstream_switch: bool,
    ) {
        if !self.playlist_state.has_size_map(variant)
            && let Err(e) =
                Self::calculate_size_map(&self.playlist_state, &self.fetch, variant).await
        {
            debug!(?e, variant, "failed to calculate variant size map");
            return;
        }

        let total = self.playlist_state.total_variant_size(variant).unwrap_or(0);
        if total == 0 {
            return;
        }
        debug!(variant, total, "refreshing variant total bytes");

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
            self.coord.timeline().set_total_bytes(Some(effective_total));
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
        Self::populate_cached_segments(
            &self.segments,
            &self.coord,
            &self.fetch,
            &self.playlist_state,
            variant,
        )
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

        let current_download = self.coord.timeline().download_position();
        if cached_end_offset > current_download {
            self.coord
                .timeline()
                .set_download_position(cached_end_offset);
        }
        if cached_count > self.current_segment_index() {
            self.advance_current_segment_index(cached_count);
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

        let switch_byte = self.coord.timeline().download_position();
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
        if self.coord.timeline().is_flushing() {
            tokio::task::yield_now().await;
            return None;
        }

        let req = self.next_valid_demand_request()?;
        trace!(
            variant = req.variant,
            segment_index = req.segment_index,
            "processing on-demand segment request"
        );

        let (req, num_segments) = self.num_segments_for_demand(req)?;
        if Self::demand_request_out_of_range(&req, num_segments) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.is_below_switch_floor(req.variant, req.segment_index) {
            debug!(
                variant = req.variant,
                segment_index = req.segment_index,
                floor = self.gap_scan_start_segment(),
                "dropping demand below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded at stale offset, refreshing demand request",
            "segment already loaded, skipping",
        ) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        let Some((is_variant_switch, is_midstream_switch)) = self
            .prepare_variant_for_demand(req.variant, req.segment_index)
            .await
        else {
            self.coord.clear_pending_segment_request(req);
            return None;
        };

        if self.should_skip_pre_switch_variant(req.variant, req.segment_index, is_midstream_switch)
        {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        if self.segment_loaded_for_demand(
            req.variant,
            req.segment_index,
            "segment loaded with stale offset after metadata calc, refreshing",
            "segment loaded from cache after metadata calc",
        ) {
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
            return None;
        }

        Some(self.build_demand_plan(&req, is_variant_switch))
    }

    fn next_valid_demand_request(&mut self) -> Option<SegmentRequest> {
        loop {
            let req = self.coord.take_segment_request()?;
            let current_epoch = self.coord.timeline().seek_epoch();
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
            self.coord.clear_pending_segment_request(req);
            self.coord.condvar.notify_all();
        }
    }

    fn num_segments_for_demand(&mut self, req: SegmentRequest) -> Option<(SegmentRequest, usize)> {
        if let Some(value) = self.num_segments(req.variant) {
            return Some((req, value));
        }

        self.publish_download_error(
            "missing variant in playlist state for demand",
            &HlsError::VariantNotFound(format!("variant {}", req.variant)),
        );
        self.coord.requeue_segment_request(req);
        self.coord.condvar.notify_all();
        None
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

        let loaded_segment = {
            let segments = self.segments.lock_sync();
            segments
                .find_loaded_segment(variant, segment_index)
                .cloned()
        };
        let Some(seg) = loaded_segment else {
            return false;
        };

        if !self.segment_resources_available(&seg) {
            debug!(
                variant,
                segment_index, "segment metadata present but resources evicted, need re-download"
            );
            return false;
        }

        trace!(
            variant,
            segment_index,
            reason = loaded_reason,
            "demand segment already loaded"
        );
        true
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
                self.coord.condvar.notify_all();
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
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) || is_midstream_switch {
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
            byte_offset: self.coord.timeline().download_position(),
        });

        let has_init = self.variant_has_init(req.variant);
        let need_init = has_init
            && (self.force_init_for_seek
                || should_request_init(
                    self.switch_needs_init(req.variant, is_variant_switch),
                    req.segment_index,
                ));
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
        if self.coord.timeline().is_flushing() || self.coord.timeline().is_seek_pending() {
            tokio::task::yield_now().await;
            return PlanOutcome::Idle;
        }

        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let variant = self.abr.get_current_variant_index();

        let Some(num_segments) = self.num_segments_for_plan(variant) else {
            return PlanOutcome::Idle;
        };

        if self.handle_tail_state(variant, num_segments) {
            return PlanOutcome::Idle;
        }

        self.publish_variant_applied(old_variant, variant, &decision);

        let (is_variant_switch, is_midstream_switch) = match self
            .ensure_variant_ready(variant, self.current_segment_index())
            .await
        {
            Ok(flags) => flags,
            Err(e) => {
                self.coord.condvar.notify_all();
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
            let advanced = batch_end > self.current_segment_index();
            self.advance_current_segment_index(batch_end);
            self.coord.condvar.notify_all();
            if advanced {
                self.coord.reader_advanced.notify_one();
            }
            return PlanOutcome::Idle;
        }

        PlanOutcome::Batch(plans)
    }

    fn num_segments_for_plan(&mut self, variant: usize) -> Option<usize> {
        if let Some(value) = self.num_segments(variant) {
            return Some(value);
        }

        self.publish_download_error(
            "missing variant in playlist state",
            &HlsError::VariantNotFound(format!("variant {variant}")),
        );
        self.coord.condvar.notify_all();
        None
    }

    fn handle_tail_state(&mut self, variant: usize, num_segments: usize) -> bool {
        if self.current_segment_index() < num_segments {
            return false;
        }

        let timeline_seek_epoch = self.coord.timeline().seek_epoch();
        if timeline_seek_epoch != self.active_seek_epoch {
            self.coord.timeline().set_eof(false);
            self.coord.condvar.notify_all();
            return true;
        }

        if self.coord.had_midstream_switch.load(Ordering::Acquire) {
            let rewind_variant = self.rewind_reference_variant(variant);
            if self.rewind_to_first_missing_segment(rewind_variant, num_segments) {
                return true;
            }
        }

        let stream_end = self
            .coord
            .timeline()
            .total_bytes()
            .or_else(|| self.playlist_state.total_variant_size(variant))
            .unwrap_or_else(|| self.segments.lock_sync().max_end_offset());
        let playback_at_end =
            stream_end == 0 || self.coord.timeline().byte_position() >= stream_end;
        if !playback_at_end {
            self.coord.timeline().set_eof(false);
            self.coord.condvar.notify_all();
            return true;
        }

        if !self.coord.timeline().eof() {
            debug!("reached end of playlist");
            self.coord.timeline().set_eof(true);
            self.bus.publish(HlsEvent::EndOfStream);
        }
        self.coord.condvar.notify_all();
        true
    }

    fn rewind_reference_variant(&self, fallback_variant: usize) -> usize {
        let current_variant = self.abr.get_current_variant_index();
        if current_variant < self.playlist_state.num_variants() {
            current_variant
        } else {
            fallback_variant
        }
    }

    fn rewind_to_first_missing_segment(&mut self, variant: usize, num_segments: usize) -> bool {
        let missing_segment = {
            let segments = self.segments.lock_sync();
            first_missing_segment(
                &segments,
                variant,
                self.gap_scan_start_segment(),
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
        self.rewind_current_segment_index(segment_index);
        self.coord.condvar.notify_all();
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
        let reader_seg = self.reader_segment_hint(variant);
        let post_seek_probe_only = self.active_seek_epoch > 0
            && self.coord.timeline().seek_epoch() == self.active_seek_epoch
            && reader_seg == self.gap_scan_start_segment()
            && self.current_segment_index() > reader_seg;
        if post_seek_probe_only {
            return (Vec::new(), self.current_segment_index());
        }

        let mut batch_end = (self.current_segment_index() + self.prefetch_count).min(num_segments);
        if let Some(limit) = self.look_ahead_segments {
            batch_end = batch_end.min(reader_seg.saturating_add(limit).saturating_add(1));
        }
        let seek_epoch = self.coord.timeline().seek_epoch();
        if self.active_seek_epoch > 0 && seek_epoch == self.active_seek_epoch {
            batch_end = batch_end.min(self.current_segment_index().saturating_add(1));
        }
        let mut plans = Vec::new();
        let has_init = self.variant_has_init(variant);
        let mut need_init = has_init
            && (self.force_init_for_seek || self.switch_needs_init(variant, is_variant_switch));

        for seg_idx in self.current_segment_index()..batch_end {
            if self.should_skip_planned_segment(variant, seg_idx, is_midstream_switch) {
                continue;
            }

            self.bus.publish(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: self.coord.timeline().download_position(),
            });

            let plan_need_init = has_init && should_request_init(need_init, seg_idx);
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

        if !plans.is_empty() && plans.len() <= 4 {
            let first = plans.first().map_or(0, |plan| plan.segment_index);
            let last = plans.last().map_or(0, |plan| plan.segment_index);
            debug!(
                variant,
                current_segment_index = self.current_segment_index(),
                batch_end,
                first_segment = first,
                last_segment = last,
                count = plans.len(),
                "built batch plans"
            );
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

        let loaded_segment = {
            let segments = self.segments.lock_sync();
            segments.find_loaded_segment(variant, seg_idx).cloned()
        };
        if let Some(seg) = loaded_segment {
            if self.segment_resources_available(&seg) {
                self.advance_current_segment_index(seg_idx + 1);
                return true;
            }
            debug!(
                variant,
                segment_index = seg_idx,
                "segment in plan window lost resources, forcing refresh"
            );
            return false;
        }

        let had_switch = self.coord.had_midstream_switch.load(Ordering::Acquire);
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
        self.coord.stopped.store(true, Ordering::Release);
        self.coord.condvar.notify_all();
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
        let current_epoch = self.coord.timeline().seek_epoch();
        if is_stale_epoch(fetch.seek_epoch, current_epoch) {
            trace!(
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
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: fetch.segment_index,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_stale_cross_codec_fetch(&fetch) {
            debug!(
                variant = fetch.variant,
                segment_index = fetch.segment_index,
                current_variant = self.last_committed_variant,
                "dropping stale cross-codec fetch after switched anchor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: fetch.segment_index,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_below_switch_floor(fetch.variant, fetch.segment_index) {
            debug!(
                variant = fetch.variant,
                segment_index = fetch.segment_index,
                floor = self.gap_scan_start_segment(),
                "dropping fetch below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: fetch.segment_index,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
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
        if fetch.segment_index == self.current_segment_index() {
            self.advance_current_segment_index(fetch.segment_index + 1);
        }

        if fetch.segment_index <= 8 {
            debug!(
                variant = fetch.variant,
                segment_index = fetch.segment_index,
                current_segment_index = self.current_segment_index(),
                "committing fetch"
            );
        }

        self.coord.clear_pending_segment_request(SegmentRequest {
            segment_index: fetch.segment_index,
            variant: fetch.variant,
            seek_epoch: fetch.seek_epoch,
        });
        self.commit_segment(fetch, is_variant_switch, is_midstream_switch);
    }

    fn should_throttle(&self) -> bool {
        // Never throttle during seek — downloader must be free to respond.
        if self.coord.timeline().is_flushing() {
            return false;
        }

        let current_variant = self.abr.get_current_variant_index();
        {
            let segments = self.segments.lock_sync();
            if !segments.is_segment_loaded(current_variant, self.current_segment_index()) {
                return false;
            }
        }

        // Byte-based throttle
        if let Some(limit) = self.look_ahead_bytes {
            let reader_pos = self.coord.timeline().byte_position();
            let downloaded = self.segments.lock_sync().max_end_offset();
            if downloaded.saturating_sub(reader_pos) > limit {
                return true;
            }
        }

        // Segment-based throttle (critical for ephemeral with small LRU cache)
        if let Some(limit) = self.look_ahead_segments {
            let reader_seg = self.reader_segment_hint(current_variant);
            if self.current_segment_index() > reader_seg + limit {
                return true;
            }
        }

        false
    }

    async fn wait_ready(&self) {
        if self.coord.timeline().is_flushing() || !self.should_throttle() {
            return;
        }
        self.coord.notified_reader_advanced().await;
    }

    fn wait_for_work(&self) -> impl Future<Output = ()> {
        self.coord.notified_reader_advanced()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{Arc, atomic::Ordering},
        time::Duration,
    };

    use kithara_assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::time::Instant;
    use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
    use kithara_stream::{AudioCodec, Downloader, PlanOutcome, Timeline};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        AbrDecision, AbrReason, DownloadState, HlsDownloader, HlsFetch, LoadedSegment,
        classify_variant_transition, first_missing_segment, is_cross_codec_switch, is_stale_epoch,
        should_request_init,
    };
    use crate::{
        config::HlsConfig,
        coord::SegmentRequest,
        fetch::{DefaultFetchManager, FetchManager, SegmentMeta},
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistAccess, PlaylistState, SegmentState, VariantSizeMap, VariantState},
        source::build_pair,
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
    async fn tail_state_uses_committed_variant_for_missing_segment_rewind() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 2),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 2),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(2);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-1").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }

        downloader
            .coord
            .abr_variant_index
            .store(0, Ordering::Release);
        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(200);
        assert!(downloader.handle_tail_state(1, 2));
        assert!(
            downloader.coord.timeline().eof(),
            "playlist should reach EOF when committed variant has no missing tail gaps"
        );
        assert_eq!(downloader.current_segment_index(), 2);
        assert_eq!(downloader.last_committed_variant, Some(0));
    }

    #[kithara::test(tokio)]
    async fn tail_state_keeps_eof_false_when_playback_not_at_end() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            2,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(2);
        downloader
            .coord
            .abr_variant_index
            .store(0, Ordering::Release);
        downloader.coord.timeline().set_byte_position(0);
        downloader.coord.timeline().set_eof(false);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }

        assert!(downloader.handle_tail_state(0, 2));
        assert!(
            !downloader.coord.timeline().eof(),
            "downloader must not set eof while playback position is not at stream end"
        );
    }

    #[kithara::test(tokio)]
    async fn should_not_throttle_when_current_variant_has_gap_to_fill() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.look_ahead_bytes = Some(100);
        downloader.look_ahead_segments = Some(1);
        downloader.cursor.reset_fill(0);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        downloader.coord.timeline().set_byte_position(0);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-0").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-1").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 2,
                byte_offset: 200,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-2").expect("valid URL"),
            });
        }

        assert!(
            !downloader.should_throttle(),
            "downloader must not throttle while the current variant still has a missing gap"
        );
    }

    #[kithara::test(tokio)]
    async fn tail_state_ignores_stale_committed_variant() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);
        downloader.cursor.reset_fill(3);
        downloader
            .coord
            .abr_variant_index
            .store(0, Ordering::Release);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 2,
                byte_offset: 200,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }

        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(300);
        assert!(downloader.handle_tail_state(1, 3));
        assert!(
            downloader.coord.timeline().eof(),
            "playlist should emit EOF when stale committed variant is unrelated to tail gaps"
        );
        assert_eq!(downloader.current_segment_index(), 3);
        assert_eq!(
            downloader.coord.abr_variant_index.load(Ordering::Acquire),
            0
        );
    }

    #[kithara::test(tokio)]
    async fn tail_state_does_not_rewind_to_uncommitted_variant() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 2),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 2),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(2);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }

        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(200);
        assert!(downloader.handle_tail_state(1, 2));
        assert!(
            downloader.coord.timeline().eof(),
            "playlist should reach EOF when tail is committed on different variant"
        );
        assert_eq!(downloader.current_segment_index(), 2);
    }

    #[kithara::test(tokio)]
    async fn tail_state_does_not_rewind_from_tail_when_playback_not_at_end() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(3);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-1-0").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 2,
                byte_offset: 200,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }
        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(150);

        assert!(downloader.handle_tail_state(1, 3));
        assert!(
            !downloader.coord.timeline().eof(),
            "tail handler must not force EOF while playback is not at stream end"
        );
        assert_eq!(downloader.current_segment_index(), 3);
    }

    #[kithara::test(tokio)]
    async fn tail_state_does_not_rewind_unseen_variant_without_midstream_switch() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reopen_fill(0, 3);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(0);

        {
            let mut segments = downloader.segments.lock_sync();
            for segment_index in 0..3 {
                segments.push(LoadedSegment {
                    variant: 0,
                    segment_index,
                    byte_offset: (segment_index as u64) * 100,
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!("https://example.com/seg-0-{segment_index}"))
                        .expect("valid segment URL"),
                });
            }
        }

        assert!(downloader.handle_tail_state(1, 3));
        assert_eq!(
            downloader.current_segment_index(),
            3,
            "tail handler must not rewind into an unseen variant before a real midstream switch"
        );
        assert!(
            !downloader.coord.timeline().eof(),
            "tail handler must keep playback alive while buffered data is still readable"
        );
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.cursor.reset_fill(1); // tail
        downloader.coord.timeline().set_eof(false);
        let _ = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(1));
        assert!(downloader.coord.timeline().is_flushing());

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Idle));
        assert!(
            !downloader.coord.timeline().eof(),
            "plan must not set EOF while seek flushing is active"
        );
    }

    #[kithara::test(tokio)]
    async fn single_segment_playlist_plans_only_segment_zero() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
            0,
            Some(AudioCodec::AacLc),
            1,
            100,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let outcome = Downloader::plan(&mut downloader).await;
        let PlanOutcome::Batch(plans) = outcome else {
            panic!("single-segment playlist must plan exactly one batch item");
        };
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].variant, 0);
        assert_eq!(plans[0].segment_index, 0);
        assert!(
            !plans[0].need_init,
            "single-segment init-less playlist must not request synthetic init"
        );
    }

    #[kithara::test(tokio)]
    async fn single_segment_playlist_reaches_tail_after_commit() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
            0,
            Some(AudioCodec::AacLc),
            1,
            100,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.commit(HlsFetch {
            init_len: 0,
            init_url: None,
            media: SegmentMeta {
                variant: 0,
                segment_type: crate::fetch::SegmentType::Media(0),
                sequence: 0,
                url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 0,
            variant: 0,
            duration: Duration::from_millis(10),
            seek_epoch: 0,
        });

        assert_eq!(downloader.current_segment_index(), 1);
        assert_eq!(downloader.last_committed_variant, Some(0));

        downloader.coord.timeline().set_byte_position(100);

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Idle));
        assert!(
            downloader.coord.timeline().eof(),
            "single-segment playlist must reach EOF without any rewind or special-case path"
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.cursor.reset_fill(1); // tail
        downloader.coord.timeline().set_eof(false);

        let epoch = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(1));
        downloader.coord.timeline().complete_seek(epoch);
        assert!(!downloader.coord.timeline().is_flushing());
        assert_ne!(
            downloader.coord.timeline().seek_epoch(),
            downloader.active_seek_epoch
        );

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Idle));
        assert!(
            !downloader.coord.timeline().eof(),
            "plan must not emit EOF while seek epoch is newer than downloader state"
        );
    }

    #[kithara::test(tokio)]
    async fn plan_while_seek_pending_is_idle() {
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.cursor.reset_fill(1);
        let epoch = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(1));
        downloader.coord.timeline().complete_seek(epoch);

        assert!(
            downloader.coord.timeline().is_seek_pending(),
            "seek_pending must stay set until decoder applies the seek"
        );
        assert!(
            !downloader.coord.timeline().is_flushing(),
            "complete_seek clears flushing before decoder applies seek"
        );

        let outcome = Downloader::plan(&mut downloader).await;
        assert!(matches!(outcome, PlanOutcome::Idle));
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
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
        assert_eq!(downloader.gap_scan_start_segment(), 0);
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
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
        assert_eq!(downloader.gap_scan_start_segment(), 0);
    }

    #[kithara::test(tokio)]
    async fn build_demand_plan_same_codec_variant_change_does_not_request_init() {
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.sent_init_for_variant.insert(0);

        let (is_variant_switch, _is_midstream_switch) =
            downloader.classify_variant_transition(1, 5);
        let plan = downloader.build_demand_plan(
            &SegmentRequest {
                segment_index: 5,
                variant: 1,
                seek_epoch: 0,
            },
            is_variant_switch,
        );

        assert!(
            !plan.need_init,
            "same-codec variant change must not inject init into demand plan"
        );
    }

    #[kithara::test(tokio)]
    async fn build_demand_plan_initial_nonzero_segment_without_init_does_not_request_init() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            4,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let (is_variant_switch, _is_midstream_switch) =
            downloader.classify_variant_transition(0, 2);
        let plan = downloader.build_demand_plan(
            &SegmentRequest {
                segment_index: 2,
                variant: 0,
                seek_epoch: 0,
            },
            is_variant_switch,
        );

        assert!(
            !plan.need_init,
            "fresh direct demand on init-less playlist must not inject init"
        );
    }

    #[kithara::test(tokio)]
    async fn build_demand_plan_initial_segment_zero_without_init_does_not_request_init() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            4,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let (is_variant_switch, _is_midstream_switch) =
            downloader.classify_variant_transition(0, 0);
        let plan = downloader.build_demand_plan(
            &SegmentRequest {
                segment_index: 0,
                variant: 0,
                seek_epoch: 0,
            },
            is_variant_switch,
        );

        assert!(
            !plan.need_init,
            "init-less playlist must not request init on initial segment zero"
        );
    }

    #[kithara::test(tokio)]
    async fn build_batch_plans_same_codec_variant_change_keeps_metadata_layout() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.sent_init_for_variant.insert(0);
        downloader.cursor.reset_fill(5);
        downloader.prefetch_count = 2;
        let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(1, 5);
        let (plans, _batch_end) =
            downloader.build_batch_plans(1, 10, is_variant_switch, is_midstream_switch);

        assert!(
            !plans.is_empty(),
            "same-codec variant change should still build sequential plans"
        );
        assert!(
            !plans[0].need_init,
            "same-codec variant change must not inject init into batch plan"
        );
    }

    #[kithara::test(tokio)]
    async fn build_batch_plans_seek_into_init_less_playlist_starts_at_target_segment() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            6,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.prefetch_count = 2;
        downloader.reset_for_seek_epoch(1, 0, 2);
        let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(0, 2);
        let (plans, _batch_end) =
            downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch);

        assert_eq!(
            plans.first().map(|plan| plan.segment_index),
            Some(2),
            "seek batch must start from the requested segment"
        );
        assert!(
            plans.iter().all(|plan| !plan.need_init),
            "seek batch on init-less playlist must not inject init"
        );
    }

    #[kithara::test(tokio)]
    async fn build_batch_plans_initial_segment_zero_without_init_does_not_request_init() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            6,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.prefetch_count = 2;
        let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(0, 0);
        let (plans, _batch_end) =
            downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch);

        assert_eq!(
            plans.first().map(|plan| plan.segment_index),
            Some(0),
            "initial batch must start from segment zero"
        );
        assert!(
            plans.iter().all(|plan| !plan.need_init),
            "init-less playlist must not inject init into initial batch"
        );
    }

    #[kithara::test(tokio)]
    async fn build_batch_plans_suppresses_prefetch_immediately_after_seek() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            10,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                segment_sizes: vec![100; 10],
                offsets: (0..10).map(|index| index as u64 * 100).collect(),
                total: 1_000,
            },
        );
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let epoch = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(1));
        downloader.coord.timeline().complete_seek(epoch);
        downloader.reset_for_seek_epoch(epoch, 0, 5);
        downloader.cursor.reopen_fill(5, 6);
        downloader.prefetch_count = 3;
        downloader.coord.timeline().set_byte_position(500);
        let (plans, batch_end) = downloader.build_batch_plans(0, 10, false, false);
        assert!(
            plans.is_empty(),
            "post-seek planner must wait for demand-driven resume"
        );
        assert_eq!(batch_end, 6);
    }

    #[kithara::test(tokio)]
    async fn build_batch_plans_limits_parallel_prefetch_during_active_seek_epoch() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::Flac),
            20,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let epoch = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(67));
        downloader.coord.timeline().complete_seek(epoch);
        downloader.reset_for_seek_epoch(epoch, 0, 11);
        downloader.cursor.reset_fill(12);
        downloader.prefetch_count = 3;
        downloader.coord.timeline().set_byte_position(8_451_629);
        let (plans, batch_end) = downloader.build_batch_plans(0, 20, false, false);
        assert_eq!(
            plans.len(),
            1,
            "active seek epoch must not launch multiple parallel prefetches"
        );
        assert_eq!(plans[0].segment_index, 12);
        assert_eq!(batch_end, 13);
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
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
        assert_eq!(downloader.gap_scan_start_segment(), 2);
    }

    /// Regression: `reset_for_seek_epoch` must not leave `total_bytes` as `None`.
    /// Source's `wait_range` uses `total_bytes` for EOF detection. A transient
    /// `None` between reset and variant total refresh causes premature EOF or
    /// missed EOF detection.
    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_preserves_total_bytes() {
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        // Establish total_bytes before seek.
        downloader.coord.timeline().set_total_bytes(Some(2400));

        // Seek reset must not clear total_bytes.
        downloader.reset_for_seek_epoch(1, 0, 0);

        assert!(
            downloader.coord.timeline().total_bytes().is_some(),
            "total_bytes must not be None after seek reset — \
             source relies on it for EOF detection in wait_range"
        );
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_same_variant_preserves_effective_total() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::Flac),
            3,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 20,
                offsets: vec![0, 120, 220],
                segment_sizes: vec![120, 100, 100],
                total: 320,
            },
        );
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.coord.timeline().set_total_bytes(Some(240));

        downloader.reset_for_seek_epoch(1, 0, 1);

        assert_eq!(
            downloader.coord.timeline().total_bytes(),
            Some(240),
            "same-variant seek reset must preserve the existing effective total_bytes"
        );
    }

    #[kithara::test(tokio)]
    async fn refresh_variant_total_bytes_uses_effective_total_for_cached_midstream_switch() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
        ]));
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 20,
                offsets: vec![0, 120, 220],
                segment_sizes: vec![120, 100, 100],
                total: 320,
            },
        );
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.coord.timeline().set_download_position(1_000);

        downloader
            .refresh_variant_total_bytes(1, 1, true, true)
            .await;

        assert_eq!(
            downloader.coord.timeline().total_bytes(),
            Some(1_200),
            "cached size maps must still produce effective totals in the switched byte space"
        );
    }

    fn make_variant_state_with_size_map(
        id: usize,
        codec: Option<AudioCodec>,
        segment_count: usize,
        seg_size: u64,
    ) -> VariantState {
        let base = Url::parse("https://example.com/").expect("valid base URL");
        let offsets: Vec<u64> = (0..segment_count).map(|i| i as u64 * seg_size).collect();
        let total = segment_count as u64 * seg_size;
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
            size_map: Some(VariantSizeMap {
                init_size: 0,
                segment_sizes: vec![seg_size; segment_count],
                offsets,
                total,
            }),
        }
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_same_variant_keeps_watermark() {
        let cancel = CancellationToken::new();
        let seg_size = 1000;
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
            0,
            Some(AudioCodec::AacLc),
            10,
            seg_size,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.coord.timeline().set_download_position(5000);

        downloader.reset_for_seek_epoch(1, 0, 2);

        assert_eq!(
            downloader.coord.timeline().download_position(),
            5000,
            "same-variant seek must keep download_position at committed watermark"
        );
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_same_variant_forward_raises_watermark() {
        let cancel = CancellationToken::new();
        let seg_size = 1000;
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_size_map(
            0,
            Some(AudioCodec::AacLc),
            10,
            seg_size,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.coord.timeline().set_download_position(5000);

        downloader.reset_for_seek_epoch(1, 0, 7);

        assert_eq!(
            downloader.coord.timeline().download_position(),
            7000,
            "forward same-variant seek must raise download_position to target"
        );
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_cross_variant_resets_position() {
        let cancel = CancellationToken::new();
        let seg_size = 1000;
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_size_map(0, Some(AudioCodec::AacLc), 10, seg_size),
            make_variant_state_with_size_map(1, Some(AudioCodec::Flac), 10, seg_size),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.coord.timeline().set_download_position(5000);

        downloader.reset_for_seek_epoch(2, 1, 3);

        assert_eq!(
            downloader.coord.timeline().download_position(),
            3000,
            "cross-variant seek must reset download_position to target metadata offset"
        );
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

        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        {
            let mut segments = downloader.segments.lock_sync();
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

    #[kithara::test(tokio)]
    async fn loaded_segment_offset_mismatch_allows_midstream_layout_delta() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
        ]));
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200],
                segment_sizes: vec![100, 100, 100],
                total: 300,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);

        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 0,
                byte_offset: 1_000,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-0.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 2,
                byte_offset: 1_200,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-2.m4s").expect("valid URL"),
            });
        }

        assert_eq!(
            downloader.loaded_segment_offset_mismatch(1, 2),
            None,
            "midstream-switch cumulative layout must not be treated as stale metadata drift"
        );
    }

    #[kithara::test(tokio)]
    async fn loaded_segment_offset_mismatch_infers_layout_delta_after_switch_flag_clears() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
        ]));
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200],
                segment_sizes: vec![100, 100, 100],
                total: 300,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);

        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 1_000,
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 2,
                byte_offset: 1_120,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-2.m4s").expect("valid URL"),
            });
        }

        assert_eq!(
            downloader.loaded_segment_offset_mismatch(1, 2),
            None,
            "shifted layout must remain valid after the one-shot midstream flag is cleared"
        );
    }

    #[kithara::test(tokio)]
    async fn loaded_segment_offset_mismatch_does_not_infer_delta_from_single_shifted_segment() {
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        {
            let mut segments = downloader.segments.lock_sync();
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
            Some((120, 100)),
            "a lone shifted segment must still be treated as stale drift"
        );
    }

    #[kithara::test(tokio)]
    async fn loaded_segment_offset_mismatch_allows_contiguous_drifted_layout() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::Flac),
            4,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200, 300],
                segment_sizes: vec![100, 100, 100, 100],
                total: 400,
            },
        );

        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 1,
                byte_offset: 1_000,
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 2,
                byte_offset: 1_120,
                init_len: 0,
                media_len: 110,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-2.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 3,
                byte_offset: 1_230,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-3.m4s").expect("valid URL"),
            });
        }

        assert_eq!(
            downloader.loaded_segment_offset_mismatch(0, 3),
            None,
            "a segment stitched into the committed contiguous layout must not be treated as stale"
        );
    }

    #[kithara::test(tokio)]
    async fn resolve_byte_offset_uses_inferred_shifted_layout_after_flag_clears() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 4),
        ]));
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200, 300],
                segment_sizes: vec![100, 100, 100, 100],
                total: 400,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);
        downloader.coord.timeline().set_download_position(1_220);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 1_000,
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 2,
                byte_offset: 1_120,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-2.m4s").expect("valid URL"),
            });
        }

        let fetch = HlsFetch {
            init_len: 0,
            init_url: None,
            media: SegmentMeta {
                variant: 1,
                segment_type: crate::fetch::SegmentType::Media(3),
                sequence: 3,
                url: Url::parse("https://example.com/seg-1-3.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 3,
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        };

        assert_eq!(
            downloader.resolve_byte_offset(&fetch, false),
            1_220,
            "subsequent commits must preserve the inferred shifted layout after the switch flag clears"
        );
    }

    #[kithara::test(tokio)]
    async fn resolve_byte_offset_reuses_existing_loaded_offset_without_inference() {
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

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.coord.timeline().set_download_position(200);
        {
            let mut segments = downloader.segments.lock_sync();
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

        let fetch = HlsFetch {
            init_len: 0,
            init_url: None,
            media: SegmentMeta {
                variant: 0,
                segment_type: crate::fetch::SegmentType::Media(1),
                sequence: 1,
                url: Url::parse("https://example.com/seg-0-1.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 1,
            variant: 0,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        };

        assert_eq!(
            downloader.resolve_byte_offset(&fetch, false),
            120,
            "re-downloading an already loaded segment must preserve its committed byte offset"
        );
    }

    #[kithara::test(tokio)]
    async fn resolve_byte_offset_revisit_uses_shifted_segment_offset_not_eof_watermark() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 4),
        ]));
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200, 300],
                segment_sizes: vec![100, 100, 100, 100],
                total: 400,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);
        downloader.coord.timeline().set_download_position(1_340);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 1_000,
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 2,
                byte_offset: 1_120,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-2.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 3,
                byte_offset: 1_220,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-3.m4s").expect("valid URL"),
            });
        }

        let fetch = HlsFetch {
            init_len: 0,
            init_url: None,
            media: SegmentMeta {
                variant: 1,
                segment_type: crate::fetch::SegmentType::Media(2),
                sequence: 2,
                url: Url::parse("https://example.com/seg-1-2.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 2,
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        };

        assert_eq!(
            downloader.resolve_byte_offset(&fetch, false),
            1_120,
            "revisit re-download must reuse the shifted segment offset instead of appending at the EOF watermark"
        );
    }

    #[kithara::test(tokio)]
    async fn commit_drops_old_cross_codec_fetch_after_switched_anchor_is_committed() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 3),
        ]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200],
                segment_sizes: vec![100, 100, 100],
                total: 300,
            },
        );
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 20,
                offsets: vec![0, 120, 220],
                segment_sizes: vec![120, 100, 100],
                total: 320,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(1);
        downloader.coord.timeline().set_download_position(220);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid URL"),
            });
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 100,
                init_len: 20,
                media_len: 100,
                init_url: None,
                media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
            });
        }

        downloader.commit(HlsFetch {
            init_len: 0,
            init_url: None,
            media: SegmentMeta {
                variant: 0,
                segment_type: crate::fetch::SegmentType::Media(2),
                sequence: 2,
                url: Url::parse("https://example.com/seg-0-2.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 2,
            variant: 0,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        assert!(
            segments.find_loaded_segment(0, 2).is_none(),
            "old cross-codec fetch must not re-enter the switched layout after a new anchor is committed"
        );
        assert_eq!(
            downloader.last_committed_variant,
            Some(1),
            "dropping the stale fetch must preserve the committed switched variant"
        );
    }

    #[kithara::test(tokio)]
    async fn commit_keeps_first_target_cross_codec_fetch_before_new_anchor_commits() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 6),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 6),
        ]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 100, 200, 300, 400, 500],
                segment_sizes: vec![100, 100, 100, 100, 100, 100],
                total: 600,
            },
        );
        playlist_state.set_size_map(
            1,
            VariantSizeMap {
                init_size: 20,
                offsets: vec![0, 120, 220, 320, 420, 520],
                segment_sizes: vec![120, 100, 100, 100, 100, 100],
                total: 620,
            },
        );

        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(3);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        downloader.coord.timeline().set_download_position(300);
        {
            let mut segments = downloader.segments.lock_sync();
            for segment_index in 0..3 {
                segments.push(LoadedSegment {
                    variant: 0,
                    segment_index,
                    byte_offset: (segment_index as u64) * 100,
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse(&format!(
                        "https://example.com/seg-0-{segment_index}.m4s",
                    ))
                    .expect("valid URL"),
                });
            }
        }

        downloader.commit(HlsFetch {
            init_len: 20,
            init_url: Some(Url::parse("https://example.com/init-1.mp4").expect("valid URL")),
            media: SegmentMeta {
                variant: 1,
                segment_type: crate::fetch::SegmentType::Media(3),
                sequence: 3,
                url: Url::parse("https://example.com/seg-1-3.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 3,
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        let committed = segments
            .find_loaded_segment(1, 3)
            .cloned()
            .expect("first target cross-codec fetch must commit");
        assert_eq!(
            committed.byte_offset, 300,
            "first switched segment must establish the new layout anchor instead of being dropped"
        );
        assert_eq!(downloader.current_segment_index(), 4);
        assert_eq!(downloader.last_committed_variant, Some(1));
    }

    #[kithara::test(tokio)]
    async fn poll_demand_drops_same_variant_request_below_switch_floor() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::DownSwitch,
                changed: true,
            },
            Instant::now(),
        );
        downloader.last_committed_variant = Some(1);
        downloader.cursor.reopen_fill(4, 4);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        downloader.coord.requeue_segment_request(SegmentRequest {
            segment_index: 2,
            variant: 1,
            seek_epoch: downloader.coord.timeline().seek_epoch(),
        });

        let plan = downloader.poll_demand().await;
        assert!(plan.is_none(), "demand below switch floor must be dropped");
    }

    #[kithara::test(tokio)]
    async fn commit_drops_same_variant_fetch_below_switch_floor() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 10),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };
        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::DownSwitch,
                changed: true,
            },
            Instant::now(),
        );
        downloader.last_committed_variant = Some(1);
        downloader.cursor.reopen_fill(4, 4);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);

        downloader.commit(HlsFetch {
            init_len: 44,
            init_url: Some(Url::parse("https://example.com/init-1.mp4").expect("valid URL")),
            media: SegmentMeta {
                variant: 1,
                segment_type: crate::fetch::SegmentType::Media(0),
                sequence: 0,
                url: Url::parse("https://example.com/seg-1-0.m4s").expect("valid URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment_index: 0,
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        assert!(
            segments.find_loaded_segment(1, 0).is_none(),
            "fetch below switch floor must not enter switched layout"
        );
        assert_eq!(downloader.current_segment_index(), 4);
    }

    /// Regression test: `handle_midstream_switch` sets `had_midstream_switch`
    /// flag and notifies condvar so the reader can re-push drained requests.
    ///
    /// Before the fix, `handle_midstream_switch` drained all segment requests
    /// without waking the reader. The reader's on-demand epoch tracking still
    /// believed its request was pending and would never re-push, causing a
    /// deadlock.
    ///
    /// The fix adds `condvar.notify_all()` after draining and checks
    /// `had_midstream_switch` in `wait_range` to clear the pending flag,
    /// allowing the reader to re-push its on-demand request.
    #[kithara::test(tokio)]
    async fn handle_midstream_switch_notifies_condvar_for_repush() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.last_committed_variant = Some(0);
        downloader.cursor.reset_fill(2);

        use crate::coord::SegmentRequest;
        downloader.coord.requeue_segment_request(SegmentRequest {
            segment_index: 1,
            variant: 1,
            seek_epoch: 0,
        });

        // Before: had_midstream_switch is false.
        assert!(
            !downloader
                .coord
                .had_midstream_switch
                .load(Ordering::Acquire)
        );

        downloader.handle_midstream_switch(true);

        // After: all requests drained.
        assert!(
            downloader.coord.take_segment_request().is_none(),
            "handle_midstream_switch must drain all requests"
        );

        // After: had_midstream_switch flag is set.
        assert!(
            downloader
                .coord
                .had_midstream_switch
                .load(Ordering::Acquire),
            "handle_midstream_switch must set had_midstream_switch flag"
        );
        assert_eq!(
            downloader.gap_scan_start_segment(),
            2,
            "midstream switch must remember the switch point for later gap rewind"
        );
    }

    #[kithara::test(tokio)]
    async fn tail_state_rewinds_missing_segments_after_midstream_switch() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 3),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 3),
        ]));
        let variants = parsed_variants(2);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (mut downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        downloader.cursor.reopen_fill(1, 3);
        downloader.last_committed_variant = Some(0);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);

        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-1-1").expect("valid segment URL");
            segments.push(LoadedSegment {
                variant: 1,
                segment_index: 1,
                byte_offset: 100,
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            });
        }

        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(150);

        assert!(downloader.handle_tail_state(1, 3));
        assert_eq!(
            downloader.current_segment_index(),
            2,
            "tail handler must rewind to the first missing segment after the switch point"
        );
        assert!(
            !downloader.coord.timeline().eof(),
            "gap rewind after midstream switch must not force EOF"
        );
    }

    /// `segment_loaded_for_demand` must trust resource presence in ephemeral mode.
    ///
    /// When bytes are still present in the cache, demand should reuse them.
    /// When init is evicted from the LRU cache, metadata alone must not suppress
    /// a re-download.
    #[kithara::test]
    fn segment_loaded_for_demand_tracks_resource_presence_in_ephemeral_mode() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            10,
        )]));
        let variants = parsed_variants(1);
        // Use cache_capacity=5 (default for ephemeral in test_fetch_manager)
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let init_url = Url::parse("https://example.com/init-0.mp4").expect("valid init URL");
        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");

        // Write data to both init and media resources
        {
            let init_key = ResourceKey::from_url(&init_url);
            let init_res = downloader
                .fetch
                .backend()
                .acquire_resource(&init_key)
                .expect("open init resource");
            init_res.write_at(0, b"init_data").expect("write init");
            init_res.commit(None).expect("commit init");

            let media_key = ResourceKey::from_url(&media_url);
            let media_res = downloader
                .fetch
                .backend()
                .acquire_resource(&media_key)
                .expect("open media resource");
            media_res.write_at(0, b"media_data").expect("write media");
            media_res.commit(None).expect("commit media");
        }

        // Register the segment in DownloadState
        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 9,
                media_len: 10,
                init_url: Some(init_url),
                media_url,
            });
        }

        assert!(
            downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
            "segment_loaded_for_demand must return true when segment resources are present"
        );

        // Open enough other resources to evict init from LRU cache
        for i in 1..20 {
            let key = ResourceKey::new(format!("evict-{i}.m4s"));
            let res = downloader
                .fetch
                .backend()
                .acquire_resource(&key)
                .expect("open evict resource");
            res.write_at(0, b"x").expect("write");
            res.commit(None).expect("commit");
        }

        assert!(
            !downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
            "segment_loaded_for_demand must return false when init resource is evicted"
        );
    }

    #[kithara::test]
    fn segment_loaded_for_demand_requires_committed_resource_in_ephemeral_mode() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            10,
        )]));
        let variants = parsed_variants(1);
        let fetch = test_fetch_manager(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let media_url = Url::parse("https://example.com/seg-0-0.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("open media resource");
        media_res.write_at(0, b"media_data").expect("write media");

        {
            let mut segments = downloader.segments.lock_sync();
            segments.push(LoadedSegment {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
                init_len: 0,
                media_len: 10,
                init_url: None,
                media_url,
            });
        }

        assert!(
            !downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
            "segment_loaded_for_demand must not treat active resources as already loaded"
        );
    }
}
