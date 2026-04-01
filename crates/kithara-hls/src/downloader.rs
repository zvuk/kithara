//! HLS downloader: fetches segments and maintains ABR state.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use futures::future::join_all;
use kithara_abr::{
    AbrController, AbrDecision, AbrReason, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource,
};
use kithara_assets::{AssetResourceState, ResourceKey};
use kithara_events::{EventBus, HlsEvent, SeekEpoch};
use kithara_platform::{BoxFuture, time::Instant, tokio};
use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
use kithara_stream::{DownloadCursor, Downloader, DownloaderIo, PlanOutcome};
use tokio::task::yield_now as task_yield_now;
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError,
    coord::{HlsCoord, SegmentRequest},
    fetch::{DefaultFetchManager, Loader, SegmentMeta},
    ids::{SegmentId, SegmentIndex, VariantIndex},
    playlist::{PlaylistAccess, PlaylistState, VariantSizeMap},
    stream_index::{SegmentData, StreamIndex},
};

/// Maximum number of plans to log at debug level.
const MAX_LOG_PLANS: usize = 4;

/// Maximum initial segment index for verbose logging.
const VERBOSE_SEGMENT_LIMIT: usize = 8;

fn is_stale_epoch(fetch_epoch: SeekEpoch, current_epoch: SeekEpoch) -> bool {
    fetch_epoch != current_epoch
}

fn is_cross_codec_switch(
    playlist_state: &PlaylistState,
    from_variant: VariantIndex,
    to_variant: VariantIndex,
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
    state: &StreamIndex,
    variant: VariantIndex,
    start_segment: SegmentIndex,
    num_segments: usize,
) -> Option<SegmentIndex> {
    let start = start_segment.min(num_segments);
    (start..num_segments).find(|&segment_index| !state.is_segment_loaded(variant, segment_index))
}

fn classify_layout_transition(
    layout_variant: VariantIndex,
    variant: VariantIndex,
    segment_index: SegmentIndex,
) -> (bool, bool) {
    let is_variant_switch = layout_variant != variant;
    let is_midstream_switch = is_variant_switch && segment_index > 0;
    (is_variant_switch, is_midstream_switch)
}

fn should_request_init(is_variant_switch: bool, segment: SegmentId) -> bool {
    match segment {
        SegmentId::Init | SegmentId::Media(0) => true,
        SegmentId::Media(_) => is_variant_switch,
    }
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
    pub(crate) variant: VariantIndex,
    pub(crate) segment: SegmentId,
    pub(crate) need_init: bool,
    pub(crate) seek_epoch: SeekEpoch,
}

/// Result of downloading a single HLS segment.
pub(crate) struct HlsFetch {
    pub(crate) init_len: u64,
    pub(crate) init_url: Option<Url>,
    pub(crate) media: SegmentMeta,
    pub(crate) media_cached: bool,
    pub(crate) segment: SegmentId,
    pub(crate) variant: VariantIndex,
    pub(crate) duration: Duration,
    pub(crate) seek_epoch: SeekEpoch,
}

impl DownloaderIo for HlsIo {
    type Plan = HlsPlan;
    type Fetch = HlsFetch;
    type Error = HlsError;

    fn fetch(&self, plan: HlsPlan) -> BoxFuture<'_, Result<HlsFetch, HlsError>> {
        Box::pin(async move {
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

            let seg_idx = plan
                .segment
                .media_index()
                .expect("HlsIo::fetch called with non-Media segment");

            let (media_result, (init_url, init_len)) = tokio::join!(
                self.fetch.load_media_segment_with_source_for_epoch(
                    plan.variant,
                    seg_idx,
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
                segment: plan.segment,
                variant: plan.variant,
                duration,
                seek_epoch: plan.seek_epoch,
            })
        })
    }
}

/// HLS downloader: fetches segments and maintains ABR state.
pub(crate) struct HlsDownloader {
    pub(crate) active_seek_epoch: SeekEpoch,
    pub(crate) io: HlsIo,
    pub(crate) fetch: Arc<DefaultFetchManager>,
    pub(crate) playlist_state: Arc<PlaylistState>,
    pub(crate) cursor: DownloadCursor<SegmentIndex>,
    pub(crate) force_init_for_seek: bool,
    pub(crate) sent_init_for_variant: HashSet<VariantIndex>,
    pub(crate) abr: AbrController<ThroughputEstimator>,
    pub(crate) coord: Arc<HlsCoord>,
    pub(crate) segments: Arc<kithara_platform::Mutex<StreamIndex>>,
    pub(crate) bus: EventBus,
    /// Backpressure threshold (bytes). None = no byte-based backpressure.
    pub(crate) look_ahead_bytes: Option<u64>,
    /// Backpressure threshold (segments ahead of reader). None = no limit.
    /// For ephemeral backends, derived from cache capacity to prevent
    /// evicting segments the reader still needs.
    pub(crate) look_ahead_segments: Option<usize>,
    /// Max segments to download in parallel per batch.
    pub(crate) prefetch_count: usize,
    /// Variant the downloader is currently targeting. Used for transition
    /// classification instead of shared `layout_variant` to avoid racing
    /// with the decoder thread.
    pub(crate) download_variant: VariantIndex,
}

impl HlsDownloader {
    fn layout_variant(&self) -> VariantIndex {
        self.segments.lock_sync().layout_variant()
    }

    fn current_segment_index(&self) -> SegmentIndex {
        self.cursor.fill_next().unwrap_or(0)
    }

    fn num_segments(&self, variant: VariantIndex) -> Option<usize> {
        self.playlist_state.num_segments(variant)
    }

    fn effective_total_bytes(&self) -> Option<u64> {
        let total = self
            .segments
            .lock_sync()
            .effective_total(self.playlist_state.as_ref());
        (total > 0).then_some(total)
    }

    fn gap_scan_start_segment(&self) -> SegmentIndex {
        self.cursor.fill_floor().unwrap_or(0)
    }

    fn reset_cursor(&mut self, segment_index: SegmentIndex) {
        self.cursor.reset_fill(segment_index);
    }

    fn advance_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.cursor.advance_fill_to(segment_index);
    }

    fn rewind_current_segment_index(&mut self, segment_index: SegmentIndex) {
        self.cursor.rewind_fill_to(segment_index);
    }

    fn is_below_switch_floor(&self, variant: VariantIndex, segment_index: SegmentIndex) -> bool {
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) {
            return false;
        }
        // With per-variant byte maps, demand from the layout_variant must
        // always be honored (e.g. seek-to-zero after ABR switch).
        // Only filter segments for the ABR variant that are below the
        // switch point in the sequential download cursor.
        let layout = self.segments.lock_sync().layout_variant();
        if variant == layout {
            return false;
        }
        let current_variant = self.abr.get_current_variant_index();
        variant == current_variant && segment_index < self.gap_scan_start_segment()
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "layout scan must hold the StreamIndex lock while item_range walks the current map"
    )]
    fn switched_layout_anchor(&self) -> Option<(VariantIndex, u64)> {
        let variant = self.abr.get_current_variant_index();
        let anchor = {
            let segments = self.segments.lock_sync();
            let vs = segments.variant_segments(variant)?;
            vs.iter().find_map(|(segment_index, _data)| {
                <StreamIndex as kithara_stream::LayoutIndex>::item_range(
                    &segments,
                    (variant, segment_index),
                )
                .map(|range| (segment_index, range.start))
            })
        };
        let (segment_index, byte_offset) = anchor?;
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

        let seg_idx = fetch.segment.media_index().unwrap_or(0);
        let fetch_offset = self
            .playlist_state
            .segment_byte_offset(fetch.variant, seg_idx)
            .unwrap_or_else(|| self.coord.timeline().download_position());

        fetch_offset >= anchor_offset
    }

    fn classify_variant_transition(&self, variant: usize, segment_index: usize) -> (bool, bool) {
        classify_layout_transition(self.download_variant, variant, segment_index)
    }

    fn reset_for_seek_epoch(
        &mut self,
        seek_epoch: SeekEpoch,
        variant: usize,
        segment_index: usize,
    ) {
        // Use layout_variant (what the decoder reads), not ABR target.
        // ABR may have switched to a different codec variant, but the decoder
        // is still on the layout variant. Comparing against ABR target would
        // incorrectly set force_init_for_seek=true for same-codec seeks,
        // injecting init data into mid-stream segments and corrupting the
        // byte stream.
        let previous_variant = self.layout_variant();

        self.abr.lock();

        self.active_seek_epoch = seek_epoch;
        self.coord.timeline().set_eof(false);
        self.coord
            .had_midstream_switch
            .store(false, Ordering::Release);
        self.reset_cursor(segment_index);

        self.force_init_for_seek = self
            .playlist_state
            .variant_codec(previous_variant)
            .zip(self.playlist_state.variant_codec(variant))
            .is_some_and(|(from, to)| from != to);

        if previous_variant == variant {
            // Same variant: keep download_position at committed watermark.
            // Segments are still in StreamIndex — downloader will check
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

        self.download_variant = variant;

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

    fn switch_needs_init(
        &self,
        variant: usize,
        _segment_index: usize,
        is_variant_switch: bool,
    ) -> bool {
        if !is_variant_switch {
            return false;
        }

        let previous = self.layout_variant();
        previous != variant && is_cross_codec_switch(&self.playlist_state, previous, variant)
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

    fn loaded_segment_offset(&self, variant: usize, segment_index: usize) -> Option<u64> {
        let segments = self.segments.lock_sync();
        <StreamIndex as kithara_stream::LayoutIndex>::item_range(
            &segments,
            (variant, segment_index),
        )
        .map(|r| r.start)
    }

    fn expected_layout_offset(&self, variant: usize, segment_index: usize) -> Option<u64> {
        // StreamIndex computes byte offsets internally for committed segments.
        let segments = self.segments.lock_sync();
        if let Some(range) = <StreamIndex as kithara_stream::LayoutIndex>::item_range(
            &segments,
            (variant, segment_index),
        ) {
            return Some(range.start);
        }
        drop(segments);
        // Fallback to playlist metadata
        self.playlist_state
            .segment_byte_offset(variant, segment_index)
    }

    fn reader_segment_hint(&self, variant: usize) -> usize {
        let reader_offset = self.coord.timeline().byte_position();
        self.playlist_state
            .find_segment_at_offset(variant, reader_offset)
            .unwrap_or_else(|| self.current_segment_index())
    }

    fn resource_covers_len(&self, key: &ResourceKey, len: u64) -> bool {
        if len == 0 {
            return true;
        }

        match self.fetch.backend().resource_state(key) {
            Ok(AssetResourceState::Committed {
                final_len: Some(final_len),
            }) => final_len >= len,
            Ok(AssetResourceState::Committed { .. }) => self
                .fetch
                .backend()
                .open_resource(key)
                .is_ok_and(|resource| resource.contains_range(0..len)),
            _ => false,
        }
    }

    /// Check whether a segment's init+media resources are fully readable.
    fn segment_resources_available(&self, data: &SegmentData) -> bool {
        if data.init_len > 0
            && let Some(ref init_url) = data.init_url
            && !self.resource_covers_len(&ResourceKey::from_url(init_url), data.init_len)
        {
            return false;
        }
        self.resource_covers_len(&ResourceKey::from_url(&data.media_url), data.media_len)
    }

    /// Check whether a previously-committed segment's init resource has been
    /// evicted from the ephemeral cache.  When true, the next demand download
    /// must include `need_init = true` so the init resource is re-fetched.
    fn demand_init_evicted(&self, variant: usize, segment_index: usize) -> bool {
        let segments = self.segments.lock_sync();
        let Some(data) = segments.stored_segment(variant, segment_index) else {
            return false;
        };
        let init_len = data.init_len;
        if init_len == 0 {
            return false;
        }
        let Some(init_url) = data.init_url.clone() else {
            return false;
        };
        drop(segments);
        !self.resource_covers_len(&ResourceKey::from_url(&init_url), init_len)
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
        let media_lengths = join_all(media_futs).await;

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
        segments: &kithara_platform::Mutex<StreamIndex>,
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
        Self::populate_cached_segments_with_open(segments, coord, playlist_state, variant, |key| {
            backend.resource_state(key).ok()
        })
    }

    fn populate_cached_segments_with_open<F>(
        segments: &kithara_platform::Mutex<StreamIndex>,
        coord: &HlsCoord,
        playlist_state: &PlaylistState,
        variant: usize,
        mut open_status: F,
    ) -> (usize, u64)
    where
        F: FnMut(&ResourceKey) -> Option<AssetResourceState>,
    {
        let init_url = playlist_state.init_url(variant);
        let num_segments = playlist_state.num_segments(variant).unwrap_or(0);

        // If init is required, verify it's cached on disk
        if let Some(ref url) = init_url {
            let init_key = ResourceKey::from_url(url);
            let init_cached = open_status(&init_key)
                .is_some_and(|status| matches!(status, AssetResourceState::Committed { .. }));
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
                open_status(&key)
                    .and_then(|status| match status {
                        AssetResourceState::Committed { final_len } => final_len,
                        _ => None,
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        let mut count = 0usize;

        for index in 0..num_segments {
            let Some(segment_url) = playlist_state.segment_url(variant, index) else {
                break;
            };

            let key = ResourceKey::from_url(&segment_url);
            let Some(status) = open_status(&key) else {
                break; // Stop on first uncached segment
            };

            if let AssetResourceState::Committed { final_len } = status {
                let media_len = final_len.unwrap_or(0);
                if media_len == 0 {
                    break;
                }

                // Init only for the first segment. Do NOT set init_url for
                // later segments — a re-download with need_init=true would
                // inject init data mid-stream, corrupting the decoder.
                let (mut actual_init_len, mut seg_init_url) = if count == 0 {
                    (init_len, init_url.clone())
                } else {
                    (0, None)
                };

                // Preserve init_len from an existing entry — a midstream
                // ABR switch may have committed this segment with init data
                // at a non-zero index.
                {
                    let segs = segments.lock_sync();
                    if let Some(existing) = segs.stored_segment(variant, index) {
                        if existing.init_len > actual_init_len {
                            actual_init_len = existing.init_len;
                        }
                        if actual_init_len > 0 && seg_init_url.is_none() {
                            seg_init_url = existing.init_url.clone();
                        }
                    }
                }

                let data = SegmentData {
                    init_len: actual_init_len,
                    media_len,
                    init_url: seg_init_url,
                    media_url: segment_url,
                };

                segments.lock_sync().commit_segment(variant, index, data);
                count += 1;
            } else {
                break; // Not committed, stop
            }
        }

        let cumulative_offset = segments.lock_sync().max_end_offset();
        if count > 0 {
            debug!(
                variant,
                count, cumulative_offset, "pre-populated cached segments from disk"
            );
            coord.condvar.notify_all();
        }

        (count, cumulative_offset)
    }

    // NOTE: resolve_byte_offset is no longer needed — StreamIndex computes byte
    // offsets internally via commit_segment → rebuild_byte_map_from.
    /// Commit a downloaded segment to the segment index.
    ///
    /// `HlsFetch` already encodes whether this decoder-visible segment fetched init.
    fn commit_segment(
        &mut self,
        dl: HlsFetch,
        _is_variant_switch: bool,
        _is_midstream_switch: bool,
    ) {
        let seg_idx = dl.segment.media_index().unwrap_or(0);

        self.record_throughput(dl.media.len, dl.duration, dl.media.duration);

        self.bus.publish(HlsEvent::SegmentComplete {
            variant: dl.variant,
            segment_index: seg_idx,
            bytes_transferred: dl.media.len,
            cached: dl.media_cached,
            duration: dl.duration,
        });

        let fresh_init_len = dl.init_len;

        // Preserve init_len from an existing entry when re-downloading after
        // eviction.  The init resource may still be available even though it
        // was not re-fetched with this media segment.
        let actual_init_len = if fresh_init_len == 0 {
            let segments = self.segments.lock_sync();
            segments
                .stored_segment(dl.variant, seg_idx)
                .map_or(0, |existing| existing.init_len)
        } else {
            fresh_init_len
        };

        let media_len = dl.media.len;
        let actual_size = actual_init_len + media_len;

        let init_url = if actual_init_len > 0 {
            dl.init_url.or_else(|| {
                let segments = self.segments.lock_sync();
                segments
                    .stored_segment(dl.variant, seg_idx)
                    .and_then(|existing| existing.init_url.clone())
            })
        } else {
            dl.init_url
        };

        let data = SegmentData {
            init_len: actual_init_len,
            media_len,
            init_url,
            media_url: dl.media.url.clone(),
        };

        self.segments
            .lock_sync()
            .commit_segment(dl.variant, seg_idx, data);

        let end_offset = self.segments.lock_sync().max_end_offset();
        let current_download = self.coord.timeline().download_position();
        let next_download = current_download.max(end_offset);
        self.coord.timeline().set_download_position(next_download);

        self.playlist_state
            .reconcile_segment_size(dl.variant, seg_idx, actual_size);

        // Sync reconciled sizes back to StreamIndex expected_sizes so that
        // byte offsets for uncommitted segments stay accurate after DRM decrypt.
        if let Some(sizes) = self.playlist_state.segment_sizes(dl.variant) {
            self.segments
                .lock_sync()
                .set_expected_sizes(dl.variant, sizes);
        }

        self.bus.publish(HlsEvent::DownloadProgress {
            offset: next_download,
            total: None,
        });

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

        if !self.playlist_state.has_size_map(variant)
            && let Err(e) =
                Self::calculate_size_map(&self.playlist_state, &self.fetch, variant).await
        {
            debug!(?e, variant, "failed to calculate variant size map");
        }

        // Sync expected segment sizes into StreamIndex so that
        // rebuild_variant_byte_map reserves correct offsets for gaps.
        {
            let mut segments = self.segments.lock_sync();
            if let Some(sizes) = self.playlist_state.segment_sizes(variant) {
                segments.set_expected_sizes(variant, sizes);
            }
            // layout_variant is owned by the source thread via
            // format_change_segment_range(). The downloader must NOT call
            // set_layout_variant — doing so races with the decoder thread's
            // byte map queries and corrupts seek offsets.
        }
        if is_variant_switch {
            self.download_variant = variant;
        }

        let (cached_count, cached_end_offset) = self.populate_cached_segments_if_needed(variant);
        self.apply_cached_segment_progress(variant, cached_count, cached_end_offset);

        Ok((is_variant_switch, is_midstream_switch))
    }

    fn handle_midstream_switch(&mut self, is_midstream_switch: bool) {
        if !is_midstream_switch {
            return;
        }

        let old_variant = self.download_variant;
        let num_segments = self.num_segments(old_variant).unwrap_or(0);
        let cursor_pos = {
            let state = self.segments.lock_sync();
            first_missing_segment(&state, old_variant, 0, num_segments).unwrap_or(num_segments)
        };

        self.cursor.reopen_fill(cursor_pos, cursor_pos);
        self.coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        self.coord.clear_segment_requests();
        self.coord.condvar.notify_all();
    }

    fn populate_cached_segments_if_needed(&self, variant: usize) -> (usize, u64) {
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

        // Publish SegmentComplete events for segments discovered by the
        // cache scan so event consumers (tests, analytics) learn about
        // cached segments that were not re-downloaded.
        for seg_idx in 0..cached_count {
            let bytes = self
                .segments
                .lock_sync()
                .stored_segment(variant, seg_idx)
                .map_or(0, |s| s.media_len + s.init_len);
            self.bus.publish(HlsEvent::SegmentComplete {
                variant,
                segment_index: seg_idx,
                bytes_transferred: bytes,
                cached: true,
                duration: Duration::ZERO,
            });
        }
    }

    async fn poll_demand_impl(&mut self) -> Option<HlsPlan> {
        if self.coord.timeline().is_flushing() {
            task_yield_now().await;
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
        _stale_reason: &str,
        loaded_reason: &str,
    ) -> bool {
        let seg_data = {
            let segments = self.segments.lock_sync();
            if variant != segments.layout_variant() || !segments.is_visible(variant, segment_index)
            {
                return false;
            }
            segments
                .variant_segments(variant)
                .and_then(|vs| vs.get(segment_index))
                .cloned()
        };
        let Some(data) = seg_data else {
            return false;
        };

        if !self.segment_resources_available(&data) {
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
        _segment_index: usize,
        is_midstream_switch: bool,
    ) -> bool {
        if !self.coord.had_midstream_switch.load(Ordering::Acquire) || is_midstream_switch {
            return false;
        }
        // With per-variant byte maps, demand from layout_variant must be honored.
        let layout = self.segments.lock_sync().layout_variant();
        if variant == layout {
            return false;
        }
        let current_variant = self.abr.get_current_variant_index();
        if variant == current_variant {
            return false;
        }
        debug!(
            variant,
            current_variant, "skipping stale segment from pre-switch variant"
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
                    self.switch_needs_init(req.variant, req.segment_index, is_variant_switch),
                    SegmentId::Media(req.segment_index),
                )
                || self.demand_init_evicted(req.variant, req.segment_index));
        if need_init {
            self.force_init_for_seek = false;
        }

        HlsPlan {
            variant: req.variant,
            segment: SegmentId::Media(req.segment_index),
            need_init,
            seek_epoch: req.seek_epoch,
        }
    }

    async fn plan_impl(&mut self) -> PlanOutcome<HlsPlan> {
        if self.coord.timeline().is_flushing() {
            task_yield_now().await;
            return PlanOutcome::Idle;
        }

        let old_variant = self.abr.get_current_variant_index();
        let decision = self.make_abr_decision();
        let variant = self.abr.get_current_variant_index();

        let Some(num_segments) = self.num_segments_for_plan(variant) else {
            return PlanOutcome::Idle;
        };

        self.publish_variant_applied(old_variant, variant, &decision);

        if self.handle_tail_state(variant, num_segments) {
            return PlanOutcome::Idle;
        }

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

        let old_variant_param = if old_variant != variant {
            Some(old_variant)
        } else {
            None
        };
        let is_cross_codec = old_variant_param
            .is_some_and(|ov| is_cross_codec_switch(&self.playlist_state, ov, variant));
        let (plans, batch_end) = self.build_batch_plans(
            variant,
            num_segments,
            is_variant_switch,
            is_midstream_switch,
            old_variant_param,
            is_cross_codec,
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

        // ABR switched variant while all segments of the previous variant were
        // already downloaded.  The cursor sits at the end of the old variant's
        // segment count, which equals or exceeds `num_segments` for the new
        // variant.  Without this check the downloader would stay idle forever
        // and the new variant's segments would never be fetched.
        //
        // Only rewind when the new variant has ZERO committed segments — this
        // means the ABR decision happened after all old-variant segments were
        // already downloaded (fast network / localhost).
        //
        // When the new variant already has committed segments (the normal
        // midstream flow already fetched them), do NOT rewind: downloading
        // the gap at the beginning would cause the decoder to replay from
        // offset 0 instead of continuing from the switch point.
        let current_variant = self.layout_variant();
        if current_variant != variant {
            let new_variant_empty = self
                .segments
                .lock_sync()
                .variant_segments(variant)
                .is_none_or(crate::stream_index::VariantSegments::is_empty);
            if new_variant_empty {
                debug!(
                    current_variant,
                    new_variant = variant,
                    num_segments,
                    "ABR variant switch in tail state (no segments yet); \
                     resetting cursor to segment 0"
                );
                self.reset_cursor(0);
                self.coord.condvar.notify_all();
                return false;
            }
        }

        let stream_end = self.effective_total_bytes().unwrap_or(0);
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
        debug!(
            from = old_variant,
            to = variant,
            reason = ?decision.reason,
            "publishing VariantApplied event"
        );
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
        old_variant: Option<usize>,
        is_cross_codec: bool,
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
            && (self.force_init_for_seek
                || self.switch_needs_init(
                    variant,
                    self.current_segment_index(),
                    is_variant_switch,
                ));

        for seg_idx in self.current_segment_index()..batch_end {
            if self.should_skip_planned_segment(
                variant,
                seg_idx,
                is_midstream_switch,
                old_variant,
                is_cross_codec,
            ) {
                continue;
            }

            self.bus.publish(HlsEvent::SegmentStart {
                variant,
                segment_index: seg_idx,
                byte_offset: self.coord.timeline().download_position(),
            });

            let segment = SegmentId::Media(seg_idx);
            let plan_need_init = has_init && should_request_init(need_init, segment);
            plans.push(HlsPlan {
                variant,
                segment,
                need_init: plan_need_init,
                seek_epoch,
            });
            if plan_need_init {
                self.force_init_for_seek = false;
            }
            need_init = false;
        }

        if !plans.is_empty() && plans.len() <= MAX_LOG_PLANS {
            let first = plans
                .first()
                .map_or(0, |p| p.segment.media_index().unwrap_or(0));
            let last = plans
                .last()
                .map_or(0, |p| p.segment.media_index().unwrap_or(0));
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
        old_variant: Option<usize>,
        is_cross_codec: bool,
    ) -> bool {
        // Cross-variant coverage check (COV-01, COV-02, COV-03)
        if let Some(old_var) = old_variant
            && !is_cross_codec
        {
            let old_data = {
                let segments = self.segments.lock_sync();
                if segments.is_segment_loaded(old_var, seg_idx) {
                    segments
                        .variant_segments(old_var)
                        .and_then(|vs| vs.get(seg_idx))
                        .cloned()
                } else {
                    None
                }
            };
            if let Some(data) = old_data
                && self.segment_resources_available(&data)
            {
                self.advance_current_segment_index(seg_idx + 1);
                return true;
            }
        }

        let seg_data = {
            let segments = self.segments.lock_sync();
            segments
                .variant_segments(variant)
                .and_then(|vs| vs.get(seg_idx))
                .cloned()
        };
        if let Some(data) = seg_data {
            if self.segment_resources_available(&data) {
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
        // Unlock ABR when seek is no longer pending.
        if self.abr.is_locked() && !self.coord.timeline().is_seek_pending() {
            self.abr.unlock();
        }

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
        let min_ms = self.abr.min_throughput_record_ms();
        if duration.as_millis() < min_ms {
            return;
        }

        let sample = ThroughputSample {
            bytes,
            duration,
            at: Instant::now(),
            source: ThroughputSampleSource::Network,
            content_duration,
        };

        self.abr.push_sample(sample);

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

    fn poll_demand(&mut self) -> BoxFuture<'_, Option<HlsPlan>> {
        Box::pin(async move { self.poll_demand_impl().await })
    }

    fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<HlsPlan>> {
        Box::pin(async move { self.plan_impl().await })
    }

    fn commit(&mut self, fetch: HlsFetch) {
        let current_epoch = self.coord.timeline().seek_epoch();
        let seg_idx = fetch.segment.media_index().unwrap_or(0);

        if is_stale_epoch(fetch.seek_epoch, current_epoch) {
            trace!(
                fetch_epoch = fetch.seek_epoch,
                current_epoch,
                variant = fetch.variant,
                segment_index = seg_idx,
                "dropping stale fetch before commit"
            );
            self.bus.publish(HlsEvent::StaleFetchDropped {
                seek_epoch: fetch.seek_epoch,
                current_epoch,
                variant: fetch.variant,
                segment_index: seg_idx,
            });
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_stale_cross_codec_fetch(&fetch) {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                current_variant = self.abr.get_current_variant_index(),
                "dropping stale cross-codec fetch after switched anchor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        if self.is_below_switch_floor(fetch.variant, seg_idx) {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                floor = self.gap_scan_start_segment(),
                "dropping fetch below switched-layout floor"
            );
            self.coord.clear_pending_segment_request(SegmentRequest {
                segment_index: seg_idx,
                variant: fetch.variant,
                seek_epoch: fetch.seek_epoch,
            });
            self.coord.condvar.notify_all();
            return;
        }

        let (is_variant_switch, is_midstream_switch) =
            self.classify_variant_transition(fetch.variant, seg_idx);

        if fetch.init_len > 0 {
            self.sent_init_for_variant.insert(fetch.variant);
        }

        // Only advance sequential position for the next expected segment.
        // On-demand loads and out-of-order batch results must not jump past gaps --
        // plan() handles skipping loaded segments when building the next batch.
        if seg_idx == self.current_segment_index() {
            self.advance_current_segment_index(seg_idx + 1);
        }

        if seg_idx <= VERBOSE_SEGMENT_LIMIT {
            debug!(
                variant = fetch.variant,
                segment_index = seg_idx,
                current_segment_index = self.current_segment_index(),
                "committing fetch"
            );
        }

        self.coord.clear_pending_segment_request(SegmentRequest {
            segment_index: seg_idx,
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
        if !self
            .segments
            .lock_sync()
            .is_segment_loaded(current_variant, self.current_segment_index())
        {
            return false;
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

    fn wait_ready(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.coord.timeline().is_flushing() || !self.should_throttle() {
                return;
            }
            self.coord.notified_reader_advanced().await;
        })
    }

    fn demand_signal(&self) -> BoxFuture<'static, ()> {
        let coord = Arc::clone(&self.coord);
        Box::pin(async move {
            coord.notified_reader_advanced().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use kithara_assets::{AssetResourceState, AssetStoreBuilder, ProcessChunkFn, ResourceKey};
    use kithara_drm::DecryptContext;
    use kithara_events::EventBus;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::{Mutex, time::Instant};
    use kithara_storage::{ResourceExt, ResourceStatus, StorageResource};
    use kithara_stream::{AudioCodec, Downloader, PlanOutcome, Timeline};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{
        AbrDecision, AbrReason, HlsDownloader, HlsFetch, StreamIndex, classify_layout_transition,
        first_missing_segment, is_cross_codec_switch, is_stale_epoch, should_request_init,
    };
    use crate::{
        config::HlsConfig,
        coord::SegmentRequest,
        fetch::{DefaultFetchManager, FetchManager, SegmentMeta},
        ids::SegmentId,
        parsing::{VariantId, VariantStream},
        playlist::{PlaylistAccess, PlaylistState, SegmentState, VariantSizeMap, VariantState},
        source::build_pair,
        stream_index::SegmentData,
    };

    #[kithara::test]
    fn commit_drops_stale_fetch_epoch() {
        assert!(is_stale_epoch(7, 8));
        assert!(!is_stale_epoch(9, 9));
    }

    #[kithara::test]
    fn first_missing_segment_detects_gap() {
        let media_url = Url::parse("https://example.com/seg.m4s").expect("valid URL");
        let mut state = StreamIndex::new(1, 3);
        state.commit_segment(
            0,
            0,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url: media_url.clone(),
            },
        );
        state.commit_segment(
            0,
            2,
            SegmentData {
                init_len: 0,
                media_len: 100,
                init_url: None,
                media_url,
            },
        );

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

    fn test_fetch_manager_disk(cancel: CancellationToken) -> (TempDir, Arc<DefaultFetchManager>) {
        let temp_dir = TempDir::new().expect("temp dir");
        let noop_drm: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, _ctx: &mut DecryptContext, _is_last| {
                output[..input.len()].copy_from_slice(input);
                Ok(input.len())
            });
        let backend = AssetStoreBuilder::new()
            .root_dir(temp_dir.path())
            .asset_root(Some("test"))
            .cache_capacity(std::num::NonZeroUsize::new(1).expect("non-zero"))
            .cancel(cancel.clone())
            .process_fn(noop_drm)
            .build();
        let net = HttpClient::new(NetOptions::default());
        (temp_dir, Arc::new(FetchManager::new(backend, net, cancel)))
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
        let variant = 1;
        let segment_index = 37;

        let (is_variant_switch, is_midstream_switch) =
            classify_layout_transition(variant, variant, segment_index);

        assert!(
            !is_variant_switch,
            "seek within same variant must not trigger variant-switch init path"
        );
        assert!(
            !is_midstream_switch,
            "seek within same variant must not trigger midstream switch path"
        );
    }

    #[kithara::test]
    fn classify_real_variant_change_marks_midstream_switch_only_after_segment_zero() {
        let from_variant = 0;
        let to_variant = 1;

        let (is_variant_switch, is_midstream_switch) =
            classify_layout_transition(from_variant, to_variant, 0);
        assert!(is_variant_switch);
        assert!(!is_midstream_switch);

        let (is_variant_switch, is_midstream_switch) =
            classify_layout_transition(from_variant, to_variant, 5);
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

        downloader.cursor.reset_fill(2);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-1").expect("valid segment URL");
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader
            .coord
            .abr_variant_index
            .store(0, Ordering::Release);
        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(200);
        // ABR variant (0) differs from requested variant (1): tail state
        // must exit so the downloader fetches the missing variant segments.
        assert!(
            !downloader.handle_tail_state(1, 2),
            "must exit tail state when layout != variant and new variant has missing segments"
        );
        assert_eq!(downloader.current_segment_index(), 0);
    }

    #[kithara::test(tokio)]
    async fn populate_cached_segments_does_not_hold_stream_index_lock_during_invalidation() {
        let cancel = CancellationToken::new();
        let callback_invocations = Arc::new(AtomicUsize::new(0));
        let callback_lock_free = Arc::new(AtomicBool::new(true));
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

        let (downloader, _source) = build_pair(
            Arc::clone(&fetch),
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let callback_invocations_clone = Arc::clone(&callback_invocations);
        let callback_lock_free_clone = Arc::clone(&callback_lock_free);
        let segments = Arc::clone(&downloader.segments);
        let (count, cumulative_offset) = HlsDownloader::populate_cached_segments_with_open(
            &downloader.segments,
            &downloader.coord,
            playlist_state.as_ref(),
            0,
            move |_key| {
                callback_invocations_clone.fetch_add(1, Ordering::AcqRel);
                if segments.try_lock().is_err() {
                    callback_lock_free_clone.store(false, Ordering::Release);
                }
                Some(AssetResourceState::Committed {
                    final_len: Some(16),
                })
            },
        );

        assert_eq!(count, 2);
        assert_eq!(cumulative_offset, 32);
        assert!(
            callback_invocations.load(Ordering::Acquire) >= 2,
            "cache scan must cross the open boundary for each cached segment"
        );
        assert!(
            callback_lock_free.load(Ordering::Acquire),
            "populate_cached_segments must not hold StreamIndex lock across reentrant open callbacks"
        );
    }

    /// RED: `populate_cached_segments` must not destroy init_len on a
    /// segment that was committed with init data during a midstream switch.
    ///
    /// Scenario: downloader commits segment N with init_len > 0 (ABR switch
    /// point). Later, all segments 0..=N are on disk and a cache rescan
    /// fires. The rescan assigns init_len only to count == 0 (the first
    /// segment found), so segment N gets init_len = 0, destroying the
    /// init data the decoder needs to start from the switch point.
    #[kithara::test(tokio)]
    async fn populate_cached_segments_preserves_init_on_non_zero_segment() {
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

        let (downloader, _source) = build_pair(
            Arc::clone(&fetch),
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        // Simulate midstream switch: segment 2 committed with init data.
        {
            let mut segments = downloader.segments.lock_sync();
            segments.commit_segment(
                0,
                2,
                SegmentData {
                    init_len: 44,
                    media_len: 100,
                    init_url: Some(
                        Url::parse("https://example.com/init-0.mp4").expect("valid init URL"),
                    ),
                    media_url: Url::parse("https://example.com/seg-0-2.m4s")
                        .expect("valid segment URL"),
                },
            );
        }

        // Cache rescan: all 4 segments are on disk (simulated).
        let (count, _) = HlsDownloader::populate_cached_segments_with_open(
            &downloader.segments,
            &downloader.coord,
            playlist_state.as_ref(),
            0,
            |_key| {
                Some(AssetResourceState::Committed {
                    final_len: Some(100),
                })
            },
        );

        assert_eq!(count, 4, "all segments must be populated");

        // The init_len on segment 2 must survive the rescan.
        let segments = downloader.segments.lock_sync();
        let seg2 = segments.stored_segment(0, 2).expect("segment 2 must exist");
        assert_eq!(
            seg2.init_len, 44,
            "populate_cached_segments must preserve init_len on a segment \
             that was committed with init data during a midstream switch"
        );
    }

    #[kithara::test(tokio)]
    async fn populate_cached_segments_ignores_active_disk_resource() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            1,
        )]));
        let (_temp_dir, fetch) = test_fetch_manager_disk(cancel);

        let media_url = playlist_state
            .segment_url(0, 0)
            .expect("segment URL present");
        let key = ResourceKey::from_url(&media_url);
        let active = fetch
            .backend()
            .acquire_resource_with_ctx(&key, None)
            .expect("acquire active resource");
        active.write_at(0, &[1; 16]).expect("write active bytes");

        let evict_key = ResourceKey::from_url(
            &Url::parse("https://example.com/seg-evict-0.m4s").expect("valid evict URL"),
        );
        let evict = fetch
            .backend()
            .acquire_resource_with_ctx(&evict_key, None)
            .expect("acquire evicting resource");
        evict.write_at(0, &[2; 16]).expect("write evicting bytes");

        assert_eq!(
            fetch
                .backend()
                .resource_state(&key)
                .expect("resource state"),
            AssetResourceState::Active
        );

        let segments = Mutex::new(StreamIndex::new(1, 1));
        let coord = crate::coord::HlsCoord::new(
            CancellationToken::new(),
            Timeline::new(),
            Arc::new(AtomicUsize::new(0)),
        );

        let (count, cumulative_offset) = HlsDownloader::populate_cached_segments(
            &segments,
            &coord,
            fetch.as_ref(),
            playlist_state.as_ref(),
            0,
        );

        assert_eq!(
            count, 0,
            "active disk resource must not be treated as a cached committed segment"
        );
        assert_eq!(cumulative_offset, 0);
        assert!(
            segments.lock_sync().find_at_offset(0).is_none(),
            "active disk resource must not be materialized into StreamIndex"
        );
    }

    /// RED: `populate_cached_segments_if_needed` must scan disk cache even
    /// during a variant switch (`is_variant_switch=true`).
    ///
    /// Bug C: the wrapper returns `(0, 0)` when `is_variant_switch` is true,
    /// skipping the cache scan entirely. This test commits a segment to
    /// disk, then calls the wrapper with `is_variant_switch=true` and
    /// asserts `cached_count > 0`.
    #[kithara::test(tokio)]
    async fn populate_cached_segments_if_needed_scans_on_variant_switch() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            2,
        )]));
        let variants = parsed_variants(1);
        let (_temp_dir, fetch) = test_fetch_manager_disk(cancel.clone());
        let config = HlsConfig {
            cancel: Some(cancel),
            ..HlsConfig::default()
        };

        let (downloader, _source) = build_pair(
            Arc::clone(&fetch),
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        // Commit segment 0 to disk so the cache scan can find it.
        let media_url = playlist_state
            .segment_url(0, 0)
            .expect("segment URL present");
        let key = ResourceKey::from_url(&media_url);
        let resource = fetch
            .backend()
            .acquire_resource_with_ctx(&key, None)
            .expect("acquire resource");
        resource.write_at(0, &[0xAA; 64]).expect("write bytes");
        resource.commit(Some(64)).expect("commit resource");

        // With Bug C fixed, the wrapper delegates to the inner function
        // unconditionally — no early return on variant switch.
        let (cached_count, _cached_end_offset) = downloader.populate_cached_segments_if_needed(0);

        assert!(
            cached_count > 0,
            "populate_cached_segments_if_needed must scan disk cache on variant switch, \
             but Bug C early return produces cached_count=0"
        );
    }

    /// Multi-switch V0->V1->V0: cursor must converge to 5 and
    /// `download_variant` must be V0 after the back-switch.
    ///
    /// Exercises the full component chain:
    /// `handle_midstream_switch` -> `populate_cached_segments_with_open`
    /// -> `apply_cached_segment_progress` for each switch step.
    #[kithara::test(tokio)]
    async fn multi_switch_v0_v1_v0_cursor_converges() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 5),
            make_variant_state_with_segments(1, Some(AudioCodec::AacLc), 5),
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

        // Commit V0 segments 0..5 in StreamIndex.
        {
            let mut segments = downloader.segments.lock_sync();
            for seg_idx in 0..5 {
                segments.commit_segment(
                    0,
                    seg_idx,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(&format!("https://example.com/seg-0-{seg_idx}.m4s"))
                            .expect("valid URL"),
                    },
                );
            }
        }

        // --- Switch 1: V0 -> V1 ---
        // handle_midstream_switch uses old download_variant (V0).
        // first_missing(V0) = 5 (all V0 committed), cursor = 5.
        downloader.handle_midstream_switch(true);
        assert_eq!(
            downloader.current_segment_index(),
            5,
            "after V0->V1 midstream switch, cursor must be at first_missing(V0)=5"
        );

        downloader.download_variant = 1;

        // V1 has no committed segments: cache scan returns (0, 0).
        let (cached_count_v1, cached_offset_v1) = HlsDownloader::populate_cached_segments_with_open(
            &downloader.segments,
            &downloader.coord,
            playlist_state.as_ref(),
            1,
            |_key| None, // V1 not on disk
        );
        assert_eq!(cached_count_v1, 0);
        downloader.apply_cached_segment_progress(1, cached_count_v1, cached_offset_v1);

        // State after V0->V1: cursor=5, download_variant=1.
        assert_eq!(downloader.current_segment_index(), 5);
        assert_eq!(downloader.download_variant, 1);

        // --- Switch 2: V1 -> V0 (back-switch) ---
        // handle_midstream_switch uses old download_variant (V1).
        // first_missing(V1) = 0 (V1 has nothing), cursor = 0.
        downloader.handle_midstream_switch(true);
        assert_eq!(
            downloader.current_segment_index(),
            0,
            "after V1->V0 midstream switch, cursor must be at first_missing(V1)=0"
        );

        downloader.download_variant = 0;

        // Cache scan for V0: all 5 segments are committed in StreamIndex.
        // Use mock that returns Committed for all V0 segments.
        let (cached_count_v0, cached_offset_v0) = HlsDownloader::populate_cached_segments_with_open(
            &downloader.segments,
            &downloader.coord,
            playlist_state.as_ref(),
            0,
            |_key| {
                Some(AssetResourceState::Committed {
                    final_len: Some(100),
                })
            },
        );
        assert_eq!(
            cached_count_v0, 5,
            "cache scan must find all 5 committed V0 segments"
        );

        downloader.apply_cached_segment_progress(0, cached_count_v0, cached_offset_v0);

        // Final state: cursor=5, download_variant=0.
        assert_eq!(
            downloader.download_variant, 0,
            "download_variant must converge to V0 after V0->V1->V0 multi-switch"
        );
        assert_eq!(
            downloader.current_segment_index(),
            5,
            "cursor must converge to 5 after V0->V1->V0 multi-switch \
             (cache scan advances past all committed V0 segments)"
        );
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
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
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
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse("https://example.com/seg-0-0").expect("valid URL"),
                },
            );
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse("https://example.com/seg-0-1").expect("valid URL"),
                },
            );
            segments.commit_segment(
                0,
                2,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse("https://example.com/seg-0-2").expect("valid URL"),
                },
            );
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

        downloader.cursor.reset_fill(3);
        downloader
            .coord
            .abr_variant_index
            .store(0, Ordering::Release);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                0,
                2,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(300);
        // Layout (V0) differs from requested variant (V1): tail state must
        // exit so the downloader fetches V1 segments from the beginning.
        assert!(
            !downloader.handle_tail_state(1, 3),
            "must exit tail state to download new variant segments"
        );
        assert_eq!(downloader.current_segment_index(), 0);
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

        downloader.cursor.reset_fill(2);
        downloader
            .coord
            .abr_variant_index
            .store(1, Ordering::Release);
        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader.coord.timeline().set_eof(false);
        downloader.coord.timeline().set_byte_position(200);
        // Layout (V0) differs from requested variant (V1): ABR has switched.
        // Tail state must exit so the downloader fetches V1 segments.
        assert!(
            !downloader.handle_tail_state(1, 2),
            "must exit tail state to download new variant segments"
        );
        assert_eq!(downloader.current_segment_index(), 0);
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
            // Assign all segments to variant 1 so commit_segment places them in byte_map
            segments.set_layout_variant(1);
            let media_url = Url::parse("https://example.com/seg-1-0").expect("valid segment URL");
            segments.commit_segment(
                1,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                1,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: media_url.clone(),
                },
            );
            segments.commit_segment(
                1,
                2,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
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
                segments.commit_segment(
                    0,
                    segment_index,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(&format!(
                            "https://example.com/seg-0-{segment_index}"
                        ))
                        .expect("valid segment URL"),
                    },
                );
            }
        }

        // Layout (V0) differs from requested variant (V1): ABR has switched
        // and the new variant's segments haven't been downloaded.
        // Tail state must exit so the downloader can fetch them.
        assert!(
            !downloader.handle_tail_state(1, 3),
            "must exit tail state to download new variant segments"
        );
        assert_eq!(
            downloader.current_segment_index(),
            0,
            "cursor must rewind to first missing segment of new variant"
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
        assert_eq!(plans[0].segment, SegmentId::Media(0));
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
            segment: SegmentId::Media(0),
            variant: 0,
            duration: Duration::from_millis(10),
            seek_epoch: 0,
        });

        assert_eq!(downloader.current_segment_index(), 1);
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
        assert_eq!(downloader.abr.get_current_variant_index(), 0);
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
        assert_eq!(downloader.abr.get_current_variant_index(), 1);
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

        downloader.sent_init_for_variant.insert(0);
        downloader.cursor.reset_fill(5);
        downloader.prefetch_count = 2;
        let (is_variant_switch, is_midstream_switch) = downloader.classify_variant_transition(1, 5);
        let (plans, _batch_end) = downloader.build_batch_plans(
            1,
            10,
            is_variant_switch,
            is_midstream_switch,
            Some(0),
            false,
        );

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
            downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch, None, false);

        assert_eq!(
            plans.first().map(|p| p.segment),
            Some(SegmentId::Media(2)),
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
            downloader.build_batch_plans(0, 6, is_variant_switch, is_midstream_switch, None, false);

        assert_eq!(
            plans.first().map(|p| p.segment),
            Some(SegmentId::Media(0)),
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
        let (plans, batch_end) = downloader.build_batch_plans(0, 10, false, false, None, false);
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
        let (plans, batch_end) = downloader.build_batch_plans(0, 20, false, false, None, false);
        assert_eq!(
            plans.len(),
            1,
            "active seek epoch must not launch multiple parallel prefetches"
        );
        assert_eq!(plans[0].segment, SegmentId::Media(12));
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

        downloader.reset_for_seek_epoch(3, 1, 2);

        assert!(
            downloader.force_init_for_seek,
            "cross-codec seek reset must force init for the first segment"
        );
        assert_eq!(downloader.abr.get_current_variant_index(), 1);
        assert_eq!(downloader.gap_scan_start_segment(), 2);
    }

    #[kithara::test(tokio)]
    async fn commit_preserves_init_for_first_cross_codec_seek_target_segment() {
        let cancel = CancellationToken::new();
        let base = Url::parse("https://example.com/").expect("valid base URL");
        let init_url = base.join("init-1.mp4").expect("valid init URL");
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_size_map(0, Some(AudioCodec::AacLc), 6, 100),
            VariantState {
                id: 1,
                uri: base.join("v1.m3u8").expect("valid playlist URL"),
                bandwidth: Some(128_000),
                codec: Some(AudioCodec::Flac),
                container: None,
                init_url: Some(init_url.clone()),
                segments: (0..6)
                    .map(|index| SegmentState {
                        index,
                        url: base
                            .join(&format!("seg-1-{index}.m4s"))
                            .expect("valid segment URL"),
                        duration: Duration::from_secs(4),
                        key: None,
                    })
                    .collect(),
                size_map: Some(VariantSizeMap {
                    init_size: 20,
                    offsets: vec![0, 120, 220, 320, 420, 520],
                    segment_sizes: vec![120, 100, 100, 100, 100, 100],
                    total: 620,
                }),
            },
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

        let seek_epoch = downloader
            .coord
            .timeline()
            .initiate_seek(Duration::from_secs(12));
        downloader.reset_for_seek_epoch(seek_epoch, 1, 3);
        downloader.segments.lock_sync().set_layout_variant(1);

        let (is_variant_switch, _is_midstream_switch) =
            downloader.classify_variant_transition(1, 3);
        assert!(
            !is_variant_switch,
            "after seek reset the layout already points at the target variant, so this path must not depend on switch detection"
        );

        let plan = downloader.build_demand_plan(
            &SegmentRequest {
                segment_index: 3,
                variant: 1,
                seek_epoch,
            },
            is_variant_switch,
        );
        assert!(
            plan.need_init,
            "cross-codec seek reset must still request init for the first decoder-visible segment"
        );

        downloader.commit(HlsFetch {
            init_len: 20,
            init_url: Some(init_url),
            media: SegmentMeta {
                variant: 1,
                segment_type: crate::fetch::SegmentType::Media(3),
                sequence: 3,
                url: base.join("seg-1-3.m4s").expect("valid segment URL"),
                duration: None,
                key: None,
                len: 100,
                container: None,
            },
            media_cached: false,
            segment: SegmentId::Media(3),
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch,
        });

        let segments = downloader.segments.lock_sync();
        let segment = segments
            .stored_segment(1, 3)
            .expect("target segment must be committed");
        assert_eq!(
            segment.init_len, 20,
            "the first decoder-visible segment after a cross-codec seek reset must retain init bytes even when the layout already points at the target variant"
        );
    }

    #[kithara::test(tokio)]
    async fn reset_for_seek_epoch_preserves_effective_total_without_timeline_cache() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![make_variant_state_with_segments(
            0,
            Some(AudioCodec::AacLc),
            3,
        )]));
        playlist_state.set_size_map(
            0,
            VariantSizeMap {
                init_size: 0,
                offsets: vec![0, 800, 1_600],
                segment_sizes: vec![800, 800, 800],
                total: 2_400,
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

        assert_eq!(downloader.effective_total_bytes(), Some(2_400));

        downloader.reset_for_seek_epoch(1, 0, 0);

        assert_eq!(
            downloader.effective_total_bytes(),
            Some(2_400),
            "seek reset must preserve HLS byte length through StreamIndex metadata"
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

        {
            let mut segments = downloader.segments.lock_sync();
            let media_url = Url::parse("https://example.com/seg-0-0").expect("valid segment URL");
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 20,
                    media_len: 220,
                    init_url: None,
                    media_url,
                },
            );
        }

        assert_eq!(downloader.effective_total_bytes(), Some(440));

        downloader.reset_for_seek_epoch(1, 0, 1);

        assert_eq!(
            downloader.effective_total_bytes(),
            Some(440),
            "same-variant seek reset must preserve the existing effective total"
        );
    }

    #[kithara::test(tokio)]
    async fn effective_total_bytes_derives_total_from_stream_index() {
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

        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        // Set up variant_map: variant 1 owns all segments
        downloader.segments.lock_sync().set_layout_variant(1);

        assert_eq!(
            downloader.effective_total_bytes(),
            Some(320),
            "effective total must be derived from StreamIndex (estimated variant 1 sizes)"
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
        assert!(should_request_init(false, SegmentId::Media(0)));
        assert!(!should_request_init(false, SegmentId::Media(1)));
    }

    #[kithara::test]
    fn should_request_init_only_for_segment_zero_or_variant_switch() {
        assert!(!should_request_init(false, SegmentId::Media(5)));
        assert!(should_request_init(true, SegmentId::Media(5)));
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

        downloader.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::DownSwitch,
                changed: true,
            },
            Instant::now(),
        );
        downloader.coord.timeline().set_download_position(220);
        {
            let mut segments = downloader.segments.lock_sync();
            segments.set_layout_variant(1);
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse("https://example.com/seg-0-0.m4s").expect("valid URL"),
                },
            );
            segments.commit_segment(
                1,
                1,
                SegmentData {
                    init_len: 20,
                    media_len: 100,
                    init_url: None,
                    media_url: Url::parse("https://example.com/seg-1-1.m4s").expect("valid URL"),
                },
            );
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
            segment: SegmentId::Media(2),
            variant: 0,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        assert!(
            !segments.is_segment_loaded(0, 2),
            "old cross-codec fetch must not re-enter the switched layout after a new anchor is committed"
        );
        assert_eq!(downloader.abr.get_current_variant_index(), 1);
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

        downloader.abr.apply(
            &AbrDecision {
                target_variant_index: 1,
                reason: AbrReason::DownSwitch,
                changed: true,
            },
            Instant::now(),
        );
        downloader.cursor.reset_fill(3);
        downloader
            .coord
            .had_midstream_switch
            .store(true, Ordering::Release);
        downloader.coord.timeline().set_download_position(300);
        {
            let mut segments = downloader.segments.lock_sync();
            // Set expected sizes for variant 1 so byte offsets are correct.
            if let Some(sizes) = playlist_state.segment_sizes(1) {
                segments.set_expected_sizes(1, sizes);
            }
            for segment_index in 0..3 {
                segments.commit_segment(
                    0,
                    segment_index,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(&format!(
                            "https://example.com/seg-0-{segment_index}.m4s",
                        ))
                        .expect("valid URL"),
                    },
                );
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
            segment: SegmentId::Media(3),
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        assert!(
            segments.is_segment_loaded(1, 3),
            "first target cross-codec fetch must commit"
        );
        let range = <StreamIndex as kithara_stream::LayoutIndex>::item_range(&segments, (1, 3))
            .expect("committed segment must have byte range");
        // Variant 1 expected sizes: [120, 100, 100, ...].
        // Segments 0-2 not committed for variant 1 → use expected sizes.
        // Offset = 120 + 100 + 100 = 320.
        assert_eq!(
            range.start, 320,
            "first switched segment must appear at the correct offset in variant 1's byte space"
        );
        drop(segments);
        assert_eq!(downloader.current_segment_index(), 4);
        assert_eq!(downloader.abr.get_current_variant_index(), 1);
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
            segment: SegmentId::Media(0),
            variant: 1,
            duration: Duration::from_millis(1),
            seek_epoch: 0,
        });

        let segments = downloader.segments.lock_sync();
        assert!(
            !segments.is_segment_loaded(1, 0),
            "fetch below switch floor must not enter switched layout"
        );
        drop(segments);
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

        // Commit V0 segments 0 and 1 so first_missing_segment returns Some(2).
        {
            let mut segments = downloader.segments.lock_sync();
            for seg_idx in 0..2 {
                segments.commit_segment(
                    0,
                    seg_idx,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(
                            &format!("https://example.com/v0-seg-{seg_idx}.m4s",),
                        )
                        .expect("valid URL"),
                    },
                );
            }
        }

        downloader.cursor.reset_fill(4);

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

        // Cursor advances to first missing segment in old variant (V0 has
        // segments 0 and 1 committed, so first missing is 2).
        assert_eq!(
            downloader.current_segment_index(),
            2,
            "midstream switch must advance cursor to first missing segment"
        );

        assert!(
            downloader
                .coord
                .had_midstream_switch
                .load(Ordering::Acquire),
            "had_midstream_switch must be true after handle_midstream_switch(true)"
        );
    }

    #[kithara::test]
    fn cursor_advances_to_first_missing_after_midstream_switch() {
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

        // Commit V0 segments 0 and 1 (segment 2 is missing).
        {
            let mut segments = downloader.segments.lock_sync();
            for seg_idx in 0..2 {
                segments.commit_segment(
                    0,
                    seg_idx,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(
                            &format!("https://example.com/v0-seg-{seg_idx}.m4s",),
                        )
                        .expect("valid URL"),
                    },
                );
            }
        }

        downloader.handle_midstream_switch(true);

        assert_eq!(
            downloader.current_segment_index(),
            2,
            "midstream switch must advance cursor to first missing segment in old variant"
        );
        assert_eq!(
            downloader.gap_scan_start_segment(),
            2,
            "midstream switch must advance floor to first missing segment"
        );
    }

    #[kithara::test]
    fn cursor_falls_back_to_zero_when_no_committed_segments() {
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

        // No commits -- V0 has nothing loaded.
        downloader.handle_midstream_switch(true);

        assert_eq!(
            downloader.current_segment_index(),
            0,
            "midstream switch with no committed segments must fall back to cursor 0"
        );
    }

    #[kithara::test]
    fn cursor_advances_to_num_segments_when_all_loaded() {
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

        // Commit all 3 V0 segments.
        {
            let mut segments = downloader.segments.lock_sync();
            for seg_idx in 0..3 {
                segments.commit_segment(
                    0,
                    seg_idx,
                    SegmentData {
                        init_len: 0,
                        media_len: 100,
                        init_url: None,
                        media_url: Url::parse(
                            &format!("https://example.com/v0-seg-{seg_idx}.m4s",),
                        )
                        .expect("valid URL"),
                    },
                );
            }
        }

        downloader.handle_midstream_switch(true);

        assert_eq!(
            downloader.current_segment_index(),
            3,
            "midstream switch with all segments loaded must advance cursor to num_segments"
        );
    }

    #[kithara::test]
    fn had_midstream_switch_is_true_after_handle_midstream_switch() {
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

        assert!(
            !downloader
                .coord
                .had_midstream_switch
                .load(Ordering::Acquire),
            "had_midstream_switch must be false before call"
        );

        downloader.handle_midstream_switch(true);

        assert!(
            downloader
                .coord
                .had_midstream_switch
                .load(Ordering::Acquire),
            "had_midstream_switch must be true after handle_midstream_switch(true)"
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
            segments.commit_segment(
                1,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 100,
                    init_url: None,
                    media_url,
                },
            );
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

        // Register the segment in StreamIndex
        {
            let mut segments = downloader.segments.lock_sync();
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 9,
                    media_len: 10,
                    init_url: Some(init_url),
                    media_url,
                },
            );
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
            segments.commit_segment(
                0,
                0,
                SegmentData {
                    init_len: 0,
                    media_len: 10,
                    init_url: None,
                    media_url,
                },
            );
        }

        assert!(
            !downloader.segment_loaded_for_demand(0, 0, "test_stale", "test_loaded"),
            "segment_loaded_for_demand must not treat active resources as already loaded"
        );
    }

    #[kithara::test]
    fn segment_loaded_for_demand_requires_visible_layout_entry() {
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

        let (downloader, _source) = build_pair(
            fetch,
            &variants,
            &config,
            Arc::clone(&playlist_state),
            EventBus::new(16),
        );

        let media_url =
            Url::parse("https://example.com/hidden-seg-0-1.m4s").expect("valid media URL");
        let media_key = ResourceKey::from_url(&media_url);
        let media_res = downloader
            .fetch
            .backend()
            .acquire_resource(&media_key)
            .expect("open media resource");
        media_res.write_at(0, b"hidden_media").expect("write media");
        media_res.commit(Some(12)).expect("commit media");

        {
            let mut segments = downloader.segments.lock_sync();
            segments.set_layout_variant(1);
            segments.commit_segment(
                0,
                1,
                SegmentData {
                    init_len: 0,
                    media_len: 12,
                    init_url: None,
                    media_url,
                },
            );
        }

        assert!(
            !downloader.segment_loaded_for_demand(0, 1, "test_stale", "test_loaded"),
            "per-variant storage alone must not suppress demand when the segment is not visible in the current layout"
        );
    }

    #[kithara::test(tokio)]
    async fn cross_variant_skip_prevents_redownload_of_committed_segment() {
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

        let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

        // Write media resource to asset store so segment_resources_available returns true
        {
            let media_key = ResourceKey::from_url(&media_url);
            let media_res = downloader
                .fetch
                .backend()
                .acquire_resource(&media_key)
                .expect("acquire media resource");
            media_res
                .write_at(0, &[0u8; 1000])
                .expect("write media data");
            media_res.commit(Some(1000)).expect("commit media");
        }

        // Commit segment 3 for variant 0 in StreamIndex
        {
            let mut segments = downloader.segments.lock_sync();
            segments.commit_segment(
                0,
                3,
                SegmentData {
                    init_len: 0,
                    media_len: 1000,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader.cursor.reset_fill(3);
        let skipped = downloader.should_skip_planned_segment(1, 3, true, Some(0), false);

        assert!(
            skipped,
            "cross-variant skip must return true when old variant has the segment committed and resource-available"
        );
        assert_eq!(
            downloader.current_segment_index(),
            4,
            "cross-variant skip must advance cursor past the skipped segment"
        );
    }

    #[kithara::test(tokio)]
    async fn cross_codec_switch_bypasses_cross_variant_skip() {
        let cancel = CancellationToken::new();
        let playlist_state = Arc::new(PlaylistState::new(vec![
            make_variant_state_with_segments(0, Some(AudioCodec::AacLc), 10),
            make_variant_state_with_segments(1, Some(AudioCodec::Flac), 10),
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

        let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

        // Write media resource to asset store
        {
            let media_key = ResourceKey::from_url(&media_url);
            let media_res = downloader
                .fetch
                .backend()
                .acquire_resource(&media_key)
                .expect("acquire media resource");
            media_res
                .write_at(0, &[0u8; 1000])
                .expect("write media data");
            media_res.commit(Some(1000)).expect("commit media");
        }

        // Commit segment 3 for variant 0 in StreamIndex
        {
            let mut segments = downloader.segments.lock_sync();
            segments.commit_segment(
                0,
                3,
                SegmentData {
                    init_len: 0,
                    media_len: 1000,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader.cursor.reset_fill(3);
        let skipped = downloader.should_skip_planned_segment(1, 3, true, Some(0), true);

        assert!(
            !skipped,
            "cross-codec switch must bypass cross-variant skip and re-download the segment"
        );
    }

    #[kithara::test(tokio)]
    async fn no_cross_variant_skip_when_old_variant_is_none() {
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

        let media_url = Url::parse("https://example.com/seg-0-3.m4s").expect("valid media URL");

        // Write media resource to asset store
        {
            let media_key = ResourceKey::from_url(&media_url);
            let media_res = downloader
                .fetch
                .backend()
                .acquire_resource(&media_key)
                .expect("acquire media resource");
            media_res
                .write_at(0, &[0u8; 1000])
                .expect("write media data");
            media_res.commit(Some(1000)).expect("commit media");
        }

        // Commit segment 3 for variant 0 in StreamIndex
        {
            let mut segments = downloader.segments.lock_sync();
            segments.commit_segment(
                0,
                3,
                SegmentData {
                    init_len: 0,
                    media_len: 1000,
                    init_url: None,
                    media_url,
                },
            );
        }

        downloader.cursor.reset_fill(3);
        let skipped = downloader.should_skip_planned_segment(1, 3, false, None, false);

        assert!(
            !skipped,
            "should_skip_planned_segment must not cross-variant skip when old_variant is None"
        );
    }
}
