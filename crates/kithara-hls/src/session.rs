#![forbid(unsafe_code)]

//! `kithara-io::Source` adapter for HLS.
//!
//! Goal: expose a single contiguous byte-addressable stream (with `Read+Seek` support via
//! `kithara-io::Reader`) backed by cached HLS segment files in `kithara-assets`.
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
//! Tracing:
//! - This module logs initialization, playlist/segment URLs, cache keys, wait/read mapping,
//!   and EOF behavior. Use:
//!   `RUST_LOG=kithara_hls=trace,kithara_io=trace,kithara_net=debug,kithara_storage=debug,kithara_assets=debug`
//!   to debug "no audio" / deadlocks.
//!
//! Deadlock pinpointing (temporary, ultra-early logs):
//! - There are `info!` markers before *each* potentially blocking await in the segment caching path.
//! - Run with at least `RUST_LOG=kithara_hls=info` to see them.
//!
//! This module is intentionally self-contained and does not introduce speculative helpers.

use std::ops::Range;

use async_trait::async_trait;
use bytes::Bytes;
use hls_m3u8::MediaPlaylist;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_io::{IoError as KitharaIoError, IoResult as KitharaIoResult, Source, WaitOutcome};
use kithara_storage::StreamingResourceExt;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::{
    CacheKeyGenerator, HlsError, HlsOptions, HlsResult, fetch::FetchManager,
    playlist::PlaylistManager,
};

pub struct HlsSessionSource {
    assets: AssetStore,
    playlist_manager: PlaylistManager,
    fetch_manager: FetchManager,
    master_url: Url,
    options: HlsOptions,

    /// `asset_root` is the master hash (stream-download-hls compatible).
    asset_root: String,

    /// Selected variant index for deterministic cache layout: `<master_hash>/<variant_index>/...`.
    ///
    /// NOTE: this is only a deterministic default. The effective selected variant is stored in
    /// `State::variant_index` once `ensure_state()` runs.
    variant_index: usize,

    /// Lazily initialized: resolved media playlist URL + parsed playlist + segment index.
    state: OnceCell<State>,
}

struct State {
    /// Selected variant index used for deterministic cache layout and on-demand fetch triggers.
    variant_index: usize,

    media_url: Url,
    media: MediaPlaylist<'static>,

    /// Segment descriptors resolved to absolute URLs.
    ///
    /// IMPORTANT: for fMP4, if the playlist contains `EXT-X-MAP`, we prepend the init segment
    /// as `segments[0]` (with `is_init=true`) before media segments, so the virtual file begins
    /// with initialization bytes.
    segments: Vec<SegmentDesc>,

    /// Total length in bytes.
    total_len: u64,

    /// Prefix sums of segment lengths: prefix[i] = sum(len[0..i]).
    ///
    /// Invariant: prefix.len() == segments.len() + 1, prefix[0] == 0, prefix.last() == total_len.
    prefix: Vec<u64>,
}

#[derive(Clone, Debug)]
struct SegmentDesc {
    url: Url,
    /// Cache rel_path under `<asset_root>/...` (without repeating master_hash).
    rel_path: String,
    /// Final length of the segment in bytes.
    len: u64,
    /// Whether this is an init segment (EXT-X-MAP).
    is_init: bool,
}

impl HlsSessionSource {
    /// Create a new source for a master playlist URL.
    ///
    /// This does not start any background tasks; fetching happens on demand from `wait_range`.
    pub fn new(
        master_url: Url,
        options: HlsOptions,
        assets: AssetStore,
        playlist_manager: PlaylistManager,
        fetch_manager: FetchManager,
    ) -> Self {
        let asset_root = crate::master_hash_from_url(&master_url);

        // The actual variant is selected from the fetched master playlist in `ensure_state()`.
        // Keep a deterministic default here; it will be overwritten during init.
        let variant_index = 0usize;

        debug!(
            master_url = %master_url,
            master_hash = %asset_root,
            "kithara-hls session source created"
        );

        Self {
            assets,
            playlist_manager,
            fetch_manager,
            master_url,
            options,
            asset_root,
            variant_index,
            state: OnceCell::new(),
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

                info!(
                    master_url = %self.master_url,
                    "kithara-hls session source: about to fetch master playlist"
                );

                // 1) Fetch master playlist.
                let master = self
                    .playlist_manager
                    .fetch_master_playlist(&self.master_url)
                    .await?;

                debug!(
                    variants_total = master.variant_streams.len(),
                    "kithara-hls master playlist fetched"
                );

                // 2) Choose variant index (deterministic).
                let variant_index = if let Some(selector) = &self.options.variant_stream_selector {
                    selector(&master).unwrap_or(0)
                } else {
                    self.options.abr_initial_variant_index.unwrap_or(0)
                };

                debug!(variant_index, "kithara-hls selected variant index");

                // 3) Resolve media playlist URL using the *actual* URI from the master playlist.
                let variant_uri: String = master
                    .variant_streams
                    .get(variant_index)
                    .and_then(|vs| match vs {
                        hls_m3u8::tags::VariantStream::ExtXStreamInf { uri, .. } => {
                            Some(uri.to_string())
                        }
                        hls_m3u8::tags::VariantStream::ExtXIFrame { .. } => None,
                    })
                    .ok_or_else(|| {
                        HlsError::VariantNotFound(format!("Variant index {}", variant_index))
                    })?;

                debug!(variant_index, variant_uri = %variant_uri, "kithara-hls variant URI picked");

                let media_url = self
                    .playlist_manager
                    .resolve_url(&self.master_url, &variant_uri)
                    .map_err(|e| e)?;

                debug!(media_url = %media_url, "kithara-hls media playlist URL resolved");

                info!(
                    media_url = %media_url,
                    "kithara-hls session source: about to fetch media playlist"
                );

                // 4) Fetch media playlist.
                let media = self
                    .playlist_manager
                    .fetch_media_playlist(&media_url)
                    .await?;

                debug!(
                    media_url = %media_url,
                    segments_total = media.segments.iter().count(),
                    "kithara-hls media playlist fetched"
                );

                // 5) Build segment descriptors (absolute URLs + deterministic cache rel_paths).
                let keys = CacheKeyGenerator::new(&self.master_url);

                // Optional init segment (EXT-X-MAP) for fMP4.
                let init_uri = media.segments.iter().find_map(|(_, seg)| {
                    seg.map.as_ref().and_then(|map| {
                        let s = map.to_string();
                        let start = s.find("URI=\"")? + "URI=\"".len();
                        let rest = s.get(start..)?;
                        let end = rest.find('\"')?;
                        Some(rest[..end].to_string())
                    })
                });

                if let Some(init_uri) = init_uri.as_deref() {
                    info!(
                        variant_index,
                        init_uri = %init_uri,
                        "kithara-hls session source: EXT-X-MAP init segment detected"
                    );
                } else {
                    info!(
                        variant_index,
                        "kithara-hls session source: no EXT-X-MAP init segment"
                    );
                }

                // Collect media segment URLs.
                let mut media_seg_urls: Vec<Url> = Vec::new();
                for (_, seg) in media.segments.iter() {
                    let seg_url = media_url.join(&seg.uri()).map_err(|e| {
                        HlsError::InvalidUrl(format!("Failed to resolve segment URL: {e}"))
                    })?;
                    media_seg_urls.push(seg_url);
                }

                debug!(
                    segments_total = media_seg_urls.len(),
                    "kithara-hls segment URLs resolved"
                );

                info!(
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
                    let init_url = media_url.join(&init_uri).map_err(|e| {
                        HlsError::InvalidUrl(format!("Failed to resolve init segment URL: {e}"))
                    })?;

                    // Init segment cache key: `<master_hash>/<variant_index>/init_<basename>`.
                    let full_rel = keys
                        .init_segment_rel_path_from_url(variant_index, &init_url)
                        .ok_or_else(|| {
                            HlsError::InvalidUrl("Failed to derive init segment basename".into())
                        })?;

                    let rel_path = full_rel
                        .strip_prefix(&format!("{}/", self.asset_root))
                        .unwrap_or(full_rel.as_str())
                        .to_string();

                    info!(
                        segment_index = 0,
                        segment_url = %init_url,
                        asset_root = %self.asset_root,
                        rel_path = %rel_path,
                        "kithara-hls session source: init segment key derived"
                    );

                    // We want init bytes to be available immediately for fMP4 demux, so trigger init download now.
                    let _ = self
                        .fetch_manager
                        .fetch_init_segment_resource(variant_index, &init_url)
                        .await?;

                    let len = self
                        .fetch_manager
                        .probe_content_length(&init_url)
                        .await?
                        .ok_or_else(|| {
                            HlsError::Driver("init segment Content-Length is unknown".into())
                        })?;

                    segments.push(SegmentDesc {
                        url: init_url,
                        rel_path,
                        len,
                        is_init: true,
                    });
                }

                let base_index = segments.len();
                for (i, url) in media_seg_urls.into_iter().enumerate() {
                    let out_index = base_index + i;
                    let media_index = i;

                    let full_rel = keys
                        .media_segment_rel_path_from_url(variant_index, &url)
                        .ok_or_else(|| {
                            HlsError::InvalidUrl("Failed to derive segment basename".into())
                        })?;

                    // Strip "<master_hash>/" because assets already scopes under asset_root.
                    let rel_path = full_rel
                        .strip_prefix(&format!("{}/", self.asset_root))
                        .unwrap_or(full_rel.as_str())
                        .to_string();

                    // Prefetch the first media segment during initialization.
                    //
                    // This reduces the chance of a sync consumer (e.g. `rodio::Decoder` via
                    // `kithara-io::Reader`) stalling on cold cache before the runtime has produced
                    // enough bytes beyond the init segment to let the demuxer proceed.
                    //
                    // IMPORTANT: `out_index` includes the optional init segment (EXT-X-MAP).
                    // Prefetch should be keyed off the media segment index.
                    if media_index == 0 {
                        let _ = self
                            .fetch_manager
                            .fetch_media_segment_resource(variant_index, &url)
                            .await?;
                    }

                    let len = self
                        .fetch_manager
                        .probe_content_length(&url)
                        .await?
                        .ok_or_else(|| {
                            HlsError::Driver("segment Content-Length is unknown".into())
                        })?;

                    segments.push(SegmentDesc {
                        url,
                        rel_path,
                        len,
                        is_init: false,
                    });
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

                Ok::<_, HlsError>(State {
                    variant_index,
                    media_url,
                    media,
                    segments,
                    total_len,
                    prefix,
                })
            })
            .await
    }

    async fn ensure_segment_committed(
        &self,
        variant_index: usize,
        url: &Url,
        rel_path: &str,
    ) -> HlsResult<u64> {
        // Legacy helper retained to keep diffs small; initialization no longer calls this.
        // We still trigger a best-effort fetch so reads can block on storage availability.
        info!(
            variant_index,
            segment_url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            "kithara-hls ensure_segment_committed: begin"
        );

        let _ = self
            .fetch_manager
            .fetch_media_segment_resource(variant_index, url)
            .await?;

        let len = self
            .fetch_manager
            .probe_content_length(url)
            .await?
            .ok_or_else(|| HlsError::Driver("segment Content-Length is unknown".into()))?;

        Ok(len)
    }

    async fn try_len_via_eof_probe(
        &self,
        res: &kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    ) -> HlsResult<Option<u64>> {
        // Deprecated: EOF probing is intentionally kept for now to avoid a large refactor in one step.
        // The session initialization path should prefer `FetchManager::probe_content_length(...)`.
        info!("kithara-hls len probe: about to wait_range(0..1)");
        let first = res.wait_range(0..1).await?;
        info!(?first, "kithara-hls len probe: wait_range(0..1) returned");

        if first == kithara_storage::WaitOutcome::Eof {
            return Ok(Some(0));
        }

        let mut off: u64 = 0;
        let chunk: u64 = 256 * 1024;

        loop {
            let end = off.saturating_add(chunk);
            trace!(off, end, "kithara-hls len probe: wait_range begin");
            match res.wait_range(off..end).await? {
                kithara_storage::WaitOutcome::Ready => {
                    let len = (end - off) as usize;
                    trace!(off, len, "kithara-hls len probe: read_at begin");
                    let bytes = res.read_at(off, len).await?;
                    trace!(
                        off,
                        got = bytes.len(),
                        "kithara-hls len probe: read_at done"
                    );

                    if bytes.is_empty() {
                        warn!(
                            off,
                            "kithara-hls len probe: empty-after-ready; treating as EOF-ish"
                        );
                        return Ok(Some(off));
                    }
                    off = off.saturating_add(bytes.len() as u64);
                }
                kithara_storage::WaitOutcome::Eof => return Ok(Some(off)),
            }
        }
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

    async fn wait_in_segment(
        &self,
        state: &State,
        segment_index: usize,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<WaitOutcome> {
        let seg = state
            .segments
            .get(segment_index)
            .ok_or_else(|| KitharaIoError::Source("segment index out of bounds".into()))?;

        // Trigger best-effort download for the segment before waiting on storage.
        // This ensures cold-cache reads make progress instead of waiting forever.
        if seg.is_init {
            let _ = self
                .fetch_manager
                .fetch_init_segment_resource(state.variant_index, &seg.url)
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;
        } else {
            let _ = self
                .fetch_manager
                .fetch_media_segment_resource(state.variant_index, &seg.url)
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;
        }

        let seg_key = ResourceKey::new(self.asset_root.clone(), seg.rel_path.clone());
        let cancel = CancellationToken::new();
        let res = self
            .assets
            .open_streaming_resource(&seg_key, cancel)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        let start = offset_in_segment;
        let end = offset_in_segment.saturating_add(len as u64);

        match res
            .wait_range(start..end)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?
        {
            kithara_storage::WaitOutcome::Ready => Ok(WaitOutcome::Ready),
            kithara_storage::WaitOutcome::Eof => Ok(WaitOutcome::Eof),
        }
    }

    async fn read_in_segment(
        &self,
        state: &State,
        segment_index: usize,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<Bytes> {
        let seg = state
            .segments
            .get(segment_index)
            .ok_or_else(|| KitharaIoError::Source("segment index out of bounds".into()))?;

        // Trigger best-effort download for the segment before reading from storage.
        // `kithara-io::Reader` is allowed to call `read_at` without a preceding `wait_range`
        // (it generally does `wait_range` first, but decoders may probe patterns).
        if seg.is_init {
            let _ = self
                .fetch_manager
                .fetch_init_segment_resource(state.variant_index, &seg.url)
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;
        } else {
            let _ = self
                .fetch_manager
                .fetch_media_segment_resource(state.variant_index, &seg.url)
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;
        }

        let seg_key = ResourceKey::new(self.asset_root.clone(), seg.rel_path.clone());
        let cancel = CancellationToken::new();
        let res = self
            .assets
            .open_streaming_resource(&seg_key, cancel)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        res.read_at(offset_in_segment, len)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))
    }
}

#[async_trait]
impl Source for HlsSessionSource {
    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome> {
        info!(
            start = range.start,
            end = range.end,
            len = self.len(),
            "kithara-hls session source wait_range begin"
        );

        let state = self
            .ensure_state()
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        info!(
            total_len = state.total_len,
            segments = state.segments.len(),
            variant_index = state.variant_index,
            "kithara-hls session source wait_range state ready"
        );

        if range.start >= state.total_len {
            trace!("kithara-hls wait_range -> EOF (start>=total_len)");
            return Ok(WaitOutcome::Eof);
        }

        // We only guarantee that *some* progress can be read; `kithara-io::Reader` requests a
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
            info!(
                pos,
                end,
                seg_idx,
                seg_off,
                seg_rel = %seg.rel_path,
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
            let need_usize: usize = need
                .try_into()
                .map_err(|_| KitharaIoError::Source("range too large".into()))?;

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

    async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes> {
        info!(
            offset,
            len,
            source_len = self.len(),
            "kithara-hls session source read_at begin"
        );

        if len == 0 {
            return Ok(Bytes::new());
        }

        let state = self
            .ensure_state()
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

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

        // Read within one segment only. The caller (`kithara-io::Reader`) will call again if it needs
        // more; this keeps reads simple and avoids allocating large buffers on boundary crossings.
        let seg = &state.segments[seg_idx];
        let seg_remaining = seg.len.saturating_sub(seg_off);

        info!(
            offset,
            len,
            seg_idx,
            seg_off,
            seg_rel = %seg.rel_path,
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
        let want_usize: usize = want
            .try_into()
            .map_err(|_| KitharaIoError::Source("read length too large".into()))?;

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
