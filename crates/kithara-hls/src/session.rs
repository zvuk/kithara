#![forbid(unsafe_code)]

//! `kithara-stream::io::Source` adapter for HLS.
//!
//! Goal: expose a single contiguous byte-addressable stream (with `Read+Seek` support via
//! `kithara-stream::io::Reader`) backed by cached HLS segment files in `kithara-assets`.
//!
//! Important constraints / notes:
//! - HLS is naturally segmented; random access is implemented by mapping a global byte offset
//!   to a (segment_index, offset_in_segment) pair.
//! - To support `SeekFrom::End`, we need a known total length. We compute it by fetching the
//!   media playlist up-front and summing segment sizes after download completion.
//! - This implementation is VOD-oriented: it assumes the media playlist is stable.
//! - Variant selection must use the *actual* URI from the master playlist (no `v0.m3u8` stubs).
//! - Cache keys must include the selected variant index to match the deterministic on-disk layout.
//! - If the media playlist contains an init segment (`EXT-X-MAP`), it must be included at the
//!   beginning of the virtual file so `rodio::Decoder`/Symphonia can demux fMP4 correctly.
//!
//! Prefetch behavior:
//! - When `prefetch_buffer_size` is `Some(n)`, we prefetch up to `n` segments ahead of current position.
//! - When `prefetch_buffer_size` is `None`, we prefetch all remaining segments (continuous download).
//! - Prefetch is triggered on `wait_in_segment` and respects ABR switches and seeks.

/// Selects the effective variant index to use for this session.
///
/// Kept as a small helper to ensure the selection logic is consistent in one place.
fn select_variant_index(master: &MasterPlaylist, options: &HlsOptions) -> usize {
    if let Some(selector) = &options.variant_stream_selector {
        selector(master).unwrap_or_else(|| options.abr_initial_variant_index.unwrap_or(0))
    } else {
        options.abr_initial_variant_index.unwrap_or(0)
    }
}

use std::collections::HashMap;
/// Tracing:
// - This module logs initialization, playlist/segment URLs, cache keys, wait/read mapping,
//   and EOF behavior. Use:
//   `RUST_LOG=kithara_hls=trace,kithara_stream::io=trace,kithara_net=debug,kithara_storage=debug,kithara_assets=debug`
//   to debug "no audio" / deadlocks.
//
// Deadlock pinpointing (temporary, ultra-early logs):
// - There are `info!` markers before *each* potentially blocking await in the segment caching path.
// - Run with at least `RUST_LOG=kithara_hls=info` to see them.
//
// This module is intentionally self-contained and does not introduce speculative helpers.
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_assets::{AssetResource, DiskAssetStore, EvictAssets, LeaseGuard, ResourceKey};
use kithara_storage::{StreamingResource, StreamingResourceExt, WaitOutcome};
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use tokio::{
    sync::{OnceCell, RwLock},
    time::sleep,
};
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::{
    HlsError, HlsOptions, HlsResult,
    fetch::FetchManager,
    playlist::{MasterPlaylist, PlaylistManager, VariantId},
};

pub struct HlsSessionSource {
    playlist_manager: PlaylistManager,
    fetch_manager: FetchManager,
    master_url: Url,
    options: HlsOptions,
    asset_root: String,
    state: OnceCell<State>,
    prefetch_started: OnceCell<tokio::task::JoinHandle<()>>,
    prefetch_next: Arc<AtomicUsize>,
    prefetch_window: usize,
    resource_cache: Arc<
        RwLock<
            HashMap<
                usize,
                AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>,
            >,
        >,
    >,
}

#[derive(Clone)]
struct State {
    variant_index: usize,
    segments: Vec<SegmentDesc>,
    total_len: u64,
    prefix: Vec<u64>,
}

/// Cache for opened streaming resources to avoid reopening the same segment multiple times
type ResourceCache =
    HashMap<usize, AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>>;

#[derive(Clone, Debug)]
struct SegmentDesc {
    url: Url,
    len: u64,
}

impl HlsSessionSource {
    /// Create a new source for a master playlist URL.
    ///
    /// This does not start any background tasks; fetching happens on demand from `wait_range`.
    pub fn new(
        master_url: Url,
        options: HlsOptions,
        playlist_manager: PlaylistManager,
        fetch_manager: FetchManager,
    ) -> Self {
        let asset_root = ResourceKey::asset_root_for_url(&master_url);

        let prefetch_window = options.prefetch_buffer_size.unwrap_or(1).max(1);

        Self {
            playlist_manager,
            fetch_manager,
            master_url,
            options,
            asset_root,
            state: OnceCell::new(),
            prefetch_started: OnceCell::new(),
            prefetch_next: Arc::new(AtomicUsize::new(0)),
            prefetch_window,
            resource_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn ensure_state(&self) -> HlsResult<&State> {
        self.state
            .get_or_try_init(|| async {
                debug!(
                    master_url = %self.master_url,
                    master_hash = %self.asset_root,
                    "kithara-hls session source init begin"
                );

                debug!(
                    master_url = %self.master_url,
                    "kithara-hls session source: about to fetch master playlist"
                );

                // 1) Fetch master playlist.
                let master = self
                    .playlist_manager
                    .fetch_master_playlist(&self.master_url)
                    .await?;

                debug!(
                    variants_total = master.variants.len(),
                    "kithara-hls master playlist fetched"
                );

                // 2) Choose variant index (deterministic).
                let variant_index = select_variant_index(&master, &self.options);

                debug!(variant_index, "kithara-hls selected variant index");

                // 3) Resolve media playlist URL using the *actual* URI from the parsed master playlist.
                let variant_uri: String = master
                    .variants
                    .get(variant_index)
                    .map(|v| v.uri.clone())
                    .ok_or_else(|| {
                        HlsError::VariantNotFound(format!("Variant index {}", variant_index))
                    })?;

                debug!(variant_index, variant_uri = %variant_uri, "kithara-hls variant URI picked");

                let media_url = self
                    .playlist_manager
                    .resolve_url(&self.master_url, &variant_uri)
                    .map_err(|e| e)?;

                debug!(media_url = %media_url, "kithara-hls media playlist URL resolved");

                debug!(
                    media_url = %media_url,
                    "kithara-hls session source: about to fetch media playlist"
                );

                // 4) Fetch media playlist.
                let media = self
                    .playlist_manager
                    .fetch_media_playlist(&media_url, VariantId(variant_index))
                    .await?;

                debug!(
                    media_url = %media_url,
                    segments_total = media.segments.len(),
                    "kithara-hls media playlist fetched"
                );

                // Optional init segment (EXT-X-MAP) for fMP4.
                let init_uri = media.init_segment.as_ref().map(|i| i.uri.as_str());

                if let Some(init_uri) = init_uri {
                    debug!(
                        variant_index,
                        init_uri = %init_uri,
                        "kithara-hls session source: EXT-X-MAP init segment detected"
                    );
                } else {
                    debug!(
                        variant_index,
                        "kithara-hls session source: no EXT-X-MAP init segment"
                    );
                }

                // Collect media segment URLs.
                let mut media_seg_urls: Vec<Url> = Vec::new();
                for seg in media.segments.iter() {
                    let seg_url = media_url.join(&seg.uri).map_err(|e| {
                        HlsError::InvalidUrl(format!("Failed to resolve segment URL: {e}"))
                    })?;
                    media_seg_urls.push(seg_url);
                }

                debug!(
                    segments_total = media_seg_urls.len(),
                    "kithara-hls segment URLs resolved"
                );

                debug!(
                    segments_total = media_seg_urls.len(),
                    "kithara-hls session source: building segment index (no eager downloads)"
                );

                // 6) Build segment descriptors (absolute URLs + deterministic cache rel_paths + lengths).
                //
                // IMPORTANT:
                // - We do NOT eagerly download all media segments here.
                // - For `SeekFrom::End` support we still need deterministic lengths up-front; we obtain
                //   them via lightweight HEAD `Content-Length` probes.
                // - Actual bytes are fetched on demand by `wait_in_segment` (trigger fetch) + storage wait.
                let mut segments: Vec<SegmentDesc> = Vec::with_capacity(
                    media_seg_urls.len() + if init_uri.is_some() { 1 } else { 0 },
                );

                if let Some(init_uri) = init_uri {
                    let init_url = media_url.join(init_uri).map_err(|e| {
                        HlsError::InvalidUrl(format!("Failed to resolve init segment URL: {e}"))
                    })?;

                    debug!(
                        segment_index = 0,
                        segment_url = %init_url,
                        asset_root = %self.asset_root,
                        "kithara-hls session source: init segment indexed"
                    );

                    // We want init bytes to be available immediately for fMP4 demux, so trigger init download now.
                    let init_fetch = self
                        .fetch_manager
                        .fetch_init_segment_resource(variant_index, &init_url)
                        .await?;

                    let len = init_fetch.bytes.len() as u64;

                    segments.push(SegmentDesc { url: init_url, len });
                }

                let base_index = segments.len();
                for (i, url) in media_seg_urls.into_iter().enumerate() {
                    let _out_index = base_index + i;
                    let media_index = i;

                    let len = if media_index == 0 {
                        let first_fetch = self
                            .fetch_manager
                            .fetch_media_segment_resource(variant_index, &url)
                            .await?;
                        first_fetch.bytes.len() as u64
                    } else {
                        self.fetch_manager
                            .probe_content_length(&url)
                            .await?
                            .ok_or_else(|| {
                                HlsError::Driver("segment Content-Length is unknown".into())
                            })?
                    };

                    segments.push(SegmentDesc { url, len });
                }

                // Prefix sums.
                let mut prefix: Vec<u64> = Vec::with_capacity(segments.len() + 1);
                prefix.push(0);
                let mut total_len: u64 = 0;
                for s in &segments {
                    total_len = total_len.saturating_add(s.len);
                    prefix.push(total_len);
                }

                debug!(
                    master_hash = %self.asset_root,
                    variant_id = variant_index,
                    segments = segments.len(),
                    total_len,
                    "kithara-hls session source initialized"
                );

                let state = State {
                    variant_index,
                    segments,
                    total_len,
                    prefix,
                };

                let fm_all = self.fetch_manager.clone();
                let urls: Vec<Url> = state.segments.iter().map(|s| s.url.clone()).collect();
                let vi = state.variant_index;
                let window = self.prefetch_window;
                let next = self.prefetch_next.clone();
                let cache = self.resource_cache.clone();
                let _ = self
                    .prefetch_started
                    .get_or_init(move || {
                        let fm_all = fm_all.clone();
                        let urls = urls.clone();
                        let next = next.clone();
                        let cache = cache.clone();
                        async move {
                            tokio::spawn(async move {
                                loop {
                                    let start = next.load(Ordering::Relaxed);
                                    if start >= urls.len() {
                                        break;
                                    }
                                    let end = (start + window).min(urls.len());
                                    for idx in start..end {
                                        if let Some(url) = urls.get(idx) {
                                            match fm_all.open_media_streaming_resource(vi, url).await {
                                                Ok(res) => {
                                                    {
                                                        let mut guard = cache.write().await;
                                                        guard.insert(idx, res);
                                                    }
                                                    tracing::debug!(variant = vi, segment_index = idx, %url, "prefetch window filled segment");
                                                }
                                                Err(err) => {
                                                    tracing::warn!(variant = vi, segment_index = idx, %url, %err, "prefetch window failed");
                                                }
                                            }
                                        }
                                    }
                                    if end >= urls.len() {
                                        break;
                                    }
                                    sleep(Duration::from_millis(25)).await;
                                }
                            })
                        }
                    })
                    .await;

                Ok::<_, HlsError>(state)
            })
            .await
    }

    fn locate(&self, state: &State, pos: u64) -> Option<(usize, u64)> {
        if pos >= state.total_len {
            return None;
        }

        // Binary search over prefix sums to find segment index.
        // Find largest i where prefix[i] <= pos, then segment = i, offset = pos - prefix[i].
        let mut lo: usize = 0;
        let mut hi: usize = state.prefix.len(); // = segments.len()+1

        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if state.prefix[mid] <= pos {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        if lo >= state.segments.len() {
            return None;
        }

        let seg_off = pos - state.prefix[lo];
        Some((lo, seg_off))
    }

    async fn resource_or_open(
        &self,
        state: &State,
        segment_index: usize,
    ) -> HlsResult<AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>> {
        let seg = state
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::PlaylistParse("segment index out of bounds".to_string()))?;

        if let Some(cached) = self
            .resource_cache
            .read()
            .await
            .get(&segment_index)
            .cloned()
        {
            return Ok(cached);
        }

        let opened = self
            .fetch_manager
            .open_media_streaming_resource(state.variant_index, &seg.url)
            .await?;

        let mut cache = self.resource_cache.write().await;
        cache.entry(segment_index).or_insert_with(|| opened.clone());
        Ok(opened)
    }

    async fn wait_in_segment(
        &self,
        state: &State,
        segment_index: usize,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<WaitOutcome, HlsError> {
        let seg = state.segments.get(segment_index).ok_or_else(|| {
            KitharaIoError::Source(HlsError::PlaylistParse(
                "segment index out of bounds".to_string(),
            ))
        })?;

        let start = offset_in_segment;
        let end = offset_in_segment.saturating_add(len as u64);

        info!(
            segment_index,
            offset_in_segment,
            len,
            segment_url = %seg.url,
            "HLS wait_in_segment START"
        );
        let wait_start = Instant::now();

        let res = self
            .resource_or_open(state, segment_index)
            .await
            .map_err(KitharaIoError::Source)?;

        let open_duration = wait_start.elapsed();
        info!(
            segment_index,
            open_duration_ms = open_duration.as_millis(),
            "HLS open_media_streaming_resource DONE"
        );

        // Trigger prefetch for upcoming segments
        self.prefetch_upcoming_segments(state, segment_index).await;

        let wait_start = Instant::now();
        let result = match res.wait_range(start..end).await {
            Ok(WaitOutcome::Ready) => {
                self.prefetch_next
                    .fetch_max(segment_index + 1, Ordering::Relaxed);
                Ok(WaitOutcome::Ready)
            }
            Ok(WaitOutcome::Eof) => Ok(WaitOutcome::Eof),
            Err(e) => Err(KitharaIoError::Source(HlsError::Storage(e))),
        };

        let wait_duration = wait_start.elapsed();
        info!(
            segment_index,
            wait_duration_ms = wait_duration.as_millis(),
            outcome = ?result,
            "HLS wait_range DONE"
        );

        result
    }

    /// Prefetch upcoming segments based on prefetch_buffer_size setting
    async fn prefetch_upcoming_segments(&self, state: &State, current_segment_index: usize) {
        let fm = self.fetch_manager.clone();
        let variant_index = state.variant_index;

        let Some(prefetch_size) = self.options.prefetch_buffer_size else {
            let next_index = current_segment_index + 1;
            if let Some(seg) = state.segments.get(next_index) {
                let url = seg.url.clone();
                info!(
                    segment_index = next_index,
                    segment_url = %url,
                    "HLS prefetch (continuous) spawn"
                );
                tokio::spawn(async move {
                    let _ = fm.open_media_streaming_resource(variant_index, &url).await;
                });
            }
            return;
        };

        if prefetch_size == 0 {
            return;
        }

        let start_index = current_segment_index + 1;
        let end_index = (current_segment_index + 1 + prefetch_size).min(state.segments.len());

        for idx in start_index..end_index {
            if let Some(seg) = state.segments.get(idx) {
                let url = seg.url.clone();
                let fm = self.fetch_manager.clone();
                let cache = self.resource_cache.clone();
                info!(
                    segment_index = idx,
                    segment_url = %url,
                    prefetch_buffer_size = prefetch_size,
                    "HLS prefetch (buffered) spawn"
                );
                tokio::spawn(async move {
                    match fm.open_media_streaming_resource(variant_index, &url).await {
                        Ok(res) => {
                            let mut guard = cache.write().await;
                            guard.entry(idx).or_insert(res);
                        }
                        Err(err) => {
                            warn!(segment_index = idx, segment_url = %url, %err, "HLS prefetch (buffered) failed");
                        }
                    }
                });
            }
        }
    }

    async fn read_in_segment(
        &self,
        state: &State,
        segment_index: usize,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<Bytes, HlsError> {
        let seg = state.segments.get(segment_index).ok_or_else(|| {
            KitharaIoError::Source(HlsError::PlaylistParse(
                "segment index out of bounds".to_string(),
            ))
        })?;

        info!(
            segment_index,
            offset_in_segment,
            len,
            segment_url = %seg.url,
            "HLS read_in_segment START"
        );
        let read_start = Instant::now();

        let res = self
            .resource_or_open(state, segment_index)
            .await
            .map_err(KitharaIoError::Source)?;

        let open_duration = read_start.elapsed();
        info!(
            segment_index,
            open_duration_ms = open_duration.as_millis(),
            "HLS open_media_streaming_resource for read DONE"
        );

        // Also trigger prefetch when reading
        self.prefetch_upcoming_segments(state, segment_index).await;

        let read_start = Instant::now();
        let result = res
            .read_at(offset_in_segment, len)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)));

        self.prefetch_next
            .fetch_max(segment_index + 1, std::sync::atomic::Ordering::Relaxed);

        let read_duration = read_start.elapsed();
        info!(
            segment_index,
            read_duration_ms = read_duration.as_millis(),
            bytes_read = result.as_ref().map(|b| b.len()).unwrap_or(0),
            "HLS read_at DONE"
        );

        result
    }
}

#[async_trait]
impl Source for HlsSessionSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        trace!(
            start = range.start,
            end = range.end,
            len = self.len(),
            "kithara-hls session source wait_range begin"
        );

        let state = self.ensure_state().await.map_err(KitharaIoError::Source)?;

        trace!(
            total_len = state.total_len,
            segments = state.segments.len(),
            variant_index = state.variant_index,
            "kithara-hls session source wait_range state ready"
        );

        if range.start >= state.total_len {
            trace!("kithara-hls wait_range -> EOF (start>=total_len)");
            return Ok(WaitOutcome::Eof);
        }

        // We only guarantee that *some* progress can be read; `kithara-stream::io::Reader` requests a
        // contiguous range. Ensure that requested window is fully available across segment boundaries.
        let mut pos = range.start;
        let end = range.end.min(state.total_len);

        while pos < end {
            let Some((seg_idx, seg_off)) = self.locate(state, pos) else {
                warn!(pos, "kithara-hls wait_range locate returned None -> EOF");
                return Ok(WaitOutcome::Eof);
            };

            let seg = &state.segments[seg_idx];
            let seg_remaining = seg.len.saturating_sub(seg_off);
            trace!(
                pos,
                end,
                seg_idx,
                seg_off,
                seg_len = seg.len,
                seg_remaining,
                "kithara-hls wait_range mapped"
            );

            if seg_remaining == 0 {
                warn!(
                    seg_idx,
                    seg_off, "kithara-hls wait_range mapped to empty remainder -> EOF"
                );
                return Ok(WaitOutcome::Eof);
            }

            let need = (end - pos).min(seg_remaining);
            let need_usize: usize = need.try_into().map_err(|_| {
                KitharaIoError::Source(HlsError::PlaylistParse("range too large".to_string()))
            })?;

            trace!(
                seg_idx,
                seg_off, need, "kithara-hls wait_range waiting in segment"
            );

            match self
                .wait_in_segment(state, seg_idx, seg_off, need_usize)
                .await?
            {
                WaitOutcome::Ready => {
                    pos = pos.saturating_add(need);
                }
                WaitOutcome::Eof => {
                    warn!(
                        seg_idx,
                        seg_off, need, "kithara-hls wait_range got EOF while expecting data"
                    );
                    return Ok(WaitOutcome::Eof);
                }
            }
        }

        trace!("kithara-hls wait_range -> Ready");
        Ok(WaitOutcome::Ready)
    }

    async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes, HlsError> {
        trace!(
            offset,
            len,
            source_len = self.len(),
            "kithara-hls session source read_at begin"
        );

        if len == 0 {
            return Ok(Bytes::new());
        }

        let state = self.ensure_state().await.map_err(KitharaIoError::Source)?;

        if offset >= state.total_len {
            trace!(
                offset,
                total_len = state.total_len,
                "kithara-hls read_at offset>=total_len -> empty"
            );
            return Ok(Bytes::new());
        }

        let Some((seg_idx, seg_off)) = self.locate(state, offset) else {
            warn!(offset, "kithara-hls read_at locate returned None -> empty");
            return Ok(Bytes::new());
        };

        trace!(
            offset,
            seg_idx,
            seg_off,
            seg_len = state.segments[seg_idx].len,
            "kithara-hls read_at located segment"
        );

        // Read within one segment only. The caller (`kithara-stream::io::Reader`) will call again if it needs
        // more; this keeps reads simple and avoids allocating large buffers on boundary crossings.
        let seg = &state.segments[seg_idx];
        let seg_remaining = seg.len.saturating_sub(seg_off);

        trace!(
            offset,
            len,
            seg_idx,
            seg_off,
            seg_len = seg.len,
            seg_remaining,
            "kithara-hls read_at mapped"
        );

        if seg_remaining == 0 {
            warn!(
                seg_idx,
                seg_off, "kithara-hls read_at mapped to empty remainder -> empty"
            );
            return Ok(Bytes::new());
        }

        let want = (len as u64).min(seg_remaining);
        let want_usize: usize = want.try_into().map_err(|_| {
            KitharaIoError::Source(HlsError::PlaylistParse("read length too large".to_string()))
        })?;

        let bytes = self
            .read_in_segment(state, seg_idx, seg_off, want_usize)
            .await?;

        trace!(
            seg_idx,
            seg_off,
            want,
            got = bytes.len(),
            "kithara-hls session source read_at done"
        );
        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        // We can only return Some(len) once initialized. `OnceCell` doesn't allow async in `len()`,
        // so return None until `ensure_state()` has been called at least once (e.g. by first read).
        self.state.get().map(|s| s.total_len)
    }
}
