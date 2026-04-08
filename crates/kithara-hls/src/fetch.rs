#![forbid(unsafe_code)]

//! Segment fetch layer: init/media dedup, DRM context resolution, HEAD probes.
//!
//! Playlist fetch/parse/cache lives in [`crate::playlist_cache::PlaylistCache`].
//! Network traffic flows through the unified [`Downloader`], which is the
//! sole `HttpClient` owner in production.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use kithara_assets::{AssetResource, AssetResourceState, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{MaybeSend, MaybeSync, RwLock, tokio, tokio::sync::OnceCell};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{
    ContainerFormat,
    dl::{Downloader, FetchCmd, FetchMethod, FetchResult as DlFetchResult, Priority},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError, HlsResult,
    ids::{SegmentIndex, VariantIndex},
    parsing::{EncryptionMethod, MasterPlaylist, MediaPlaylist, SegmentKey, VariantId},
    playlist::PlaylistState,
    playlist_cache::PlaylistCache,
};

/// AES-128 key length in bytes.
const AES_KEY_LEN: usize = 16;

// Types

/// Segment type: initialization segment or media segment with index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Initialization segment (fMP4 only, contains codec metadata).
    Init,
    /// Media segment with index in the playlist.
    Media(SegmentIndex),
}

impl SegmentType {
    /// Get media segment index, or None for init segment.
    #[must_use]
    pub fn media_index(self) -> Option<usize> {
        match self {
            Self::Media(idx) => Some(idx),
            Self::Init => None,
        }
    }
}

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: VariantIndex,
    pub segment_type: SegmentType,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
    pub len: u64,
    pub container: Option<ContainerFormat>,
}

/// Result of starting a fetch.
///
/// Every fetch path is now Downloader-driven: either the resource was
/// already committed in the cache (`was_cached == true`) or the unified
/// [`Downloader`] completed the download and committed the resource
/// inline inside [`FetchManager::start_fetch`] (`was_cached == false`).
pub struct FetchResult {
    pub bytes: u64,
    pub was_cached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SegmentFetchKey {
    decrypt_ctx: Option<DecryptContext>,
    key: ResourceKey,
}

#[derive(Clone, Debug)]
struct SegmentLoad {
    cached: bool,
    meta: SegmentMeta,
}

// Loader trait

/// Generic segment loader.
#[expect(async_fn_in_trait)]
#[cfg_attr(test, unimock::unimock(api = LoaderMock))]
pub trait Loader: MaybeSend + MaybeSync {
    /// Load media segment and return metadata with real size (after processing).
    async fn load_media_segment(
        &self,
        variant: VariantIndex,
        segment_index: SegmentIndex,
    ) -> HlsResult<SegmentMeta>;

    /// Load init segment (fMP4 only) with deduplication.
    async fn load_init_segment(&self, variant: VariantIndex) -> HlsResult<SegmentMeta>;

    /// Get number of variants from master playlist.
    fn num_variants(&self) -> usize;

    /// Get total segments in variant's media playlist.
    async fn num_segments(&self, variant: VariantIndex) -> HlsResult<usize>;
}

// FetchManager

/// Unified HLS segment fetch + cache coordinator.
///
/// Handles init/media segment deduplication, DRM context resolution,
/// and HEAD probes. Playlist fetch/parse/cache is delegated to an
/// inline [`PlaylistCache`]. Every network fetch routes through the
/// unified [`Downloader`], which is the sole `HttpClient` owner in
/// production.
#[derive(Clone)]
pub struct FetchManager {
    backend: AssetStore<DecryptContext>,
    /// Sole network fetcher — shared `HttpClient` / pool / runtime.
    downloader: Downloader,
    #[expect(
        dead_code,
        reason = "used by future phase 3 dedicated-thread migration"
    )]
    cancel: CancellationToken,
    key_manager: Option<Arc<crate::keys::KeyManager>>,
    headers: Option<Headers>,
    /// Playlist fetch + parse + disk cache. Shared via `Clone` — clones
    /// see the same master/media `OnceCell` state through internal `Arc`s.
    cache: PlaylistCache,
    // Init segment deduplication: first caller downloads, others wait on OnceCell
    init_segments: Arc<RwLock<HashMap<usize, Arc<OnceCell<SegmentMeta>>>>>,
    // Media segment deduplication for active network fetches only.
    media_segments: Arc<RwLock<HashMap<SegmentFetchKey, Arc<OnceCell<SegmentLoad>>>>>,
    // Parsed playlist state (populated in Hls::create after loading all media playlists)
    playlist_state: Option<Arc<PlaylistState>>,
}

impl FetchManager {
    #[must_use]
    pub fn new(
        backend: AssetStore<DecryptContext>,
        downloader: Downloader,
        cancel: CancellationToken,
    ) -> Self {
        let cache = PlaylistCache::new(backend.clone(), downloader.clone());
        Self {
            backend,
            downloader,
            cancel,
            key_manager: None,
            headers: None,
            cache,
            init_segments: Arc::new(RwLock::new(HashMap::new())),
            media_segments: Arc::new(RwLock::new(HashMap::new())),
            playlist_state: None,
        }
    }

    /// Set master playlist URL (required for Loader functionality).
    #[must_use]
    pub fn with_master_url(mut self, url: Url) -> Self {
        self.cache.set_master_url(url);
        self
    }

    /// Set base URL override for resolving relative URLs.
    #[must_use]
    pub fn with_base_url(mut self, url: Option<Url>) -> Self {
        self.cache.set_base_url(url);
        self
    }

    /// Set key manager for DRM decryption.
    #[must_use]
    pub fn with_key_manager(mut self, km: Arc<crate::keys::KeyManager>) -> Self {
        self.key_manager = Some(km);
        self
    }

    /// Set additional HTTP headers for all requests.
    #[must_use]
    pub fn with_headers(mut self, headers: Option<Headers>) -> Self {
        self.headers = headers.clone();
        self.cache.set_headers(headers);
        self
    }

    /// Merge per-request headers with config-level headers.
    /// Per-request headers take precedence on key conflict.
    fn merge_headers(&self, request_headers: Option<Headers>) -> Option<Headers> {
        match (&self.headers, request_headers) {
            (None, None) => None,
            (Some(base), None) => Some(base.clone()),
            (None, Some(req)) => Some(req),
            (Some(base), Some(req)) => {
                let mut merged = base.clone();
                for (k, v) in req.iter() {
                    merged.insert(k, v);
                }
                Some(merged)
            }
        }
    }

    /// Get the parsed playlist state (if set).
    #[must_use]
    pub fn playlist_state(&self) -> Option<&Arc<PlaylistState>> {
        self.playlist_state.as_ref()
    }

    /// Set the parsed playlist state.
    pub fn set_playlist_state(&mut self, state: Arc<PlaylistState>) {
        self.playlist_state = Some(state);
    }

    #[must_use]
    pub fn asset_root(&self) -> &str {
        self.backend.asset_root()
    }

    #[must_use]
    pub fn backend(&self) -> &AssetStore<DecryptContext> {
        &self.backend
    }

    // Playlist delegates

    /// Load and parse the master playlist.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        self.cache.master_playlist(url).await
    }

    /// Load and parse the media playlist for a specific variant.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub async fn media_playlist(
        &self,
        url: &Url,
        variant_id: VariantId,
    ) -> HlsResult<MediaPlaylist> {
        self.cache.media_playlist(url, variant_id).await
    }

    /// Resolve a possibly-relative target URL.
    ///
    /// # Errors
    /// Returns an error when URL joining fails.
    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        self.cache.resolve_url(base, target)
    }

    // Low-level fetch

    /// Fetch a key resource through the disk cache + unified downloader.
    ///
    /// # Errors
    /// Returns an error when cache access, network fetch, or URL
    /// handling fails.
    pub async fn fetch_key(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        let merged = self.merge_headers(headers);
        self.cache
            .fetch_atomic_to_store(url, rel_path, merged, "key")
            .await
    }

    /// Start fetching a segment via the unified [`Downloader`].
    ///
    /// - If the resource is already in the cache, returns
    ///   `FetchResult { was_cached: true }`.
    /// - Otherwise drives the fetch through
    ///   `Downloader::execute(FetchCmd::Stream)` with a writer callback
    ///   that writes each chunk into the `AssetResource`, commits the
    ///   resource inline on success, and returns
    ///   `FetchResult { was_cached: false }`.
    ///
    /// When `decrypt_ctx` is `Some`, the resource is opened with a
    /// decrypt context; decryption happens on the fly inside
    /// `AssetResource::write_at` via the `ProcessingAssets` layer.
    pub(crate) async fn start_fetch(
        &self,
        url: &Url,
        decrypt_ctx: Option<DecryptContext>,
    ) -> HlsResult<FetchResult> {
        let key = ResourceKey::from_url(url);
        if let AssetResourceState::Committed { final_len } = self.backend.resource_state(&key)? {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit via resource_state");
            return Ok(FetchResult {
                bytes: len,
                was_cached: true,
            });
        }

        let res = self.backend.acquire_resource_with_ctx(&key, decrypt_ctx)?;

        let status = res.status();
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit");
            return Ok(FetchResult {
                bytes: len,
                was_cached: true,
            });
        }

        trace!(url = %url, "start_fetch: downloading via Downloader");
        let bytes = self.download_stream_via_downloader(url, &res).await?;
        Ok(FetchResult {
            bytes,
            was_cached: false,
        })
    }

    /// Stream a fetch into an `AssetResource` via the unified Downloader.
    ///
    /// Issues a `FetchCmd::Stream` whose writer callback writes each chunk
    /// to the resource at the current sequential offset, then commits the
    /// resource with the final length. On zero bytes, marks the resource
    /// as failed and returns a `SegmentNotFound` error.
    async fn download_stream_via_downloader(
        &self,
        url: &Url,
        res: &AssetResource<DecryptContext>,
    ) -> HlsResult<u64> {
        let res_writer = res.clone();
        let mut offset: u64 = 0;
        let writer_cb: kithara_stream::dl::WriterFn = Box::new(move |chunk: &[u8]| {
            let pos = offset;
            offset += chunk.len() as u64;
            res_writer
                .write_at(pos, chunk)
                .map_err(std::io::Error::other)
        });
        let cmd = FetchCmd {
            method: FetchMethod::Stream,
            url: url.clone(),
            range: None,
            headers: self.headers.clone(),
            priority: Priority::Normal,
            on_connect: None,
            writer: Some(writer_cb),
            on_complete: None,
            throttle: None,
        };
        let total = match self.downloader.execute(cmd).await {
            DlFetchResult::Ok { bytes_written, .. } => bytes_written,
            DlFetchResult::Err(e) => {
                res.fail(format!("fetch failed: {e}"));
                return Err(HlsError::from(e));
            }
        };
        if total == 0 {
            res.fail(format!("0 bytes downloaded: {url}"));
            return Err(HlsError::SegmentNotFound(format!(
                "download yielded 0 bytes for {url}",
            )));
        }
        res.commit(Some(total)).map_err(HlsError::from)?;
        Ok(res.len().unwrap_or(total))
    }

    fn media_segment_cell(&self, key: SegmentFetchKey) -> Arc<OnceCell<SegmentLoad>> {
        let mut guard = self.media_segments.lock_sync_write();
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    }

    fn clear_media_segment_cell(&self, key: &SegmentFetchKey, cell: &Arc<OnceCell<SegmentLoad>>) {
        let mut guard = self.media_segments.lock_sync_write();
        if guard
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, cell))
        {
            guard.remove(key);
        }
    }

    // DRM helpers

    /// Resolve decryption context for a segment.
    ///
    /// Returns `Some(DecryptContext)` for AES-128 encrypted segments,
    /// `None` for unencrypted segments or unsupported methods.
    async fn resolve_decrypt_context(
        &self,
        key: Option<&SegmentKey>,
        segment_url: &Url,
        sequence: u64,
    ) -> HlsResult<Option<DecryptContext>> {
        let Some(seg_key) = key else {
            return Ok(None);
        };

        if !matches!(seg_key.method, EncryptionMethod::Aes128) {
            return Ok(None);
        }

        let Some(ref key_info) = seg_key.key_info else {
            return Ok(None);
        };

        let Some(ref km) = self.key_manager else {
            return Err(HlsError::KeyProcessing(
                "encrypted segment but no KeyManager configured".to_string(),
            ));
        };

        let iv = crate::keys::KeyManager::derive_iv(key_info, sequence);
        let key_url = crate::keys::KeyManager::resolve_key_url(key_info, segment_url)?;
        let raw_key = km.get_raw_key(&key_url, Some(iv)).await?;

        if raw_key.len() != AES_KEY_LEN {
            return Err(HlsError::KeyProcessing(format!(
                "invalid AES-128 key length: {}",
                raw_key.len()
            )));
        }

        let mut key_bytes = [0u8; AES_KEY_LEN];
        key_bytes.copy_from_slice(&raw_key[..AES_KEY_LEN]);

        debug!(
            url = %segment_url,
            sequence,
            "resolved DRM context for segment"
        );

        Ok(Some(DecryptContext::new(key_bytes, iv)))
    }

    // Init segment (OnceCell-deduped)

    /// Download init segment for a variant. No race recovery needed — `OnceCell`
    /// guarantees exactly one caller performs the download.
    async fn fetch_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        let (media_url, playlist) = self.cache.load_media_playlist(variant).await?;
        let container = if playlist.init_segment.is_some() {
            Some(ContainerFormat::Fmp4)
        } else {
            Some(ContainerFormat::MpegTs)
        };

        let init_segment = playlist.init_segment.as_ref().ok_or_else(|| {
            HlsError::SegmentNotFound(format!(
                "init segment not found in variant {variant} playlist",
            ))
        })?;

        let init_url = media_url.join(&init_segment.uri).map_err(|e| {
            HlsError::InvalidUrl(format!("Failed to resolve init segment URL: {e}"))
        })?;

        let decrypt_ctx = self
            .resolve_decrypt_context(init_segment.key.as_ref(), &init_url, 0)
            .await?;

        let FetchResult {
            bytes: init_len, ..
        } = self.start_fetch(&init_url, decrypt_ctx).await?;

        Ok(SegmentMeta {
            variant,
            segment_type: SegmentType::Init,
            sequence: 0,
            url: init_url,
            duration: None,
            key: None,
            len: init_len,
            container,
        })
    }

    /// Load init segment with deduplication via `OnceCell`.
    ///
    /// First caller downloads, concurrent callers wait on the same cell.
    /// Pattern matches `PlaylistCache::media_playlist()`.
    ///
    /// # Errors
    /// Returns an error when playlist loading, URL resolution, fetch, or content-length detection fails.
    pub async fn load_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        let mut cell = {
            let mut guard = self.init_segments.lock_sync_write();
            guard
                .entry(variant)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        // Ephemeral: cached init metadata may outlive the actual resource
        // (evicted from LRU). Verify and re-fetch if needed.
        if self.backend.is_ephemeral()
            && let Some(meta) = cell.get()
            && !self.backend.has_resource(&ResourceKey::from_url(&meta.url))
        {
            debug!(variant, url = %meta.url, "init resource evicted, resetting cache");
            let new_cell = Arc::new(OnceCell::new());
            self.init_segments
                .lock_sync_write()
                .insert(variant, Arc::clone(&new_cell));
            cell = new_cell;
        }

        let meta = cell
            .get_or_try_init(|| self.fetch_init_segment(variant))
            .await?;

        Ok(meta.clone())
    }

    pub(crate) async fn load_media_segment_with_source_for_epoch(
        &self,
        variant: usize,
        segment_index: usize,
        _seek_epoch: u64,
    ) -> HlsResult<(SegmentMeta, bool)> {
        let (media_url, playlist) = self.cache.load_media_playlist(variant).await?;

        let container = if playlist.init_segment.is_some() {
            Some(ContainerFormat::Fmp4)
        } else {
            Some(ContainerFormat::MpegTs)
        };

        let segment = playlist.segments.get(segment_index).ok_or_else(|| {
            HlsError::SegmentNotFound(format!(
                "segment {segment_index} not found in variant {variant} playlist",
            ))
        })?;

        let segment_url = media_url
            .join(&segment.uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {e}")))?;

        // Resolve DRM context for encrypted segments
        let decrypt_ctx = self
            .resolve_decrypt_context(segment.key.as_ref(), &segment_url, segment.sequence)
            .await?;
        let fetch_key = SegmentFetchKey {
            decrypt_ctx: decrypt_ctx.clone(),
            key: ResourceKey::from_url(&segment_url),
        };
        let cell = self.media_segment_cell(fetch_key.clone());

        let load = cell
            .get_or_try_init(|| async {
                let FetchResult {
                    bytes: segment_len,
                    was_cached: cached,
                } = self.start_fetch(&segment_url, decrypt_ctx).await?;

                Ok::<_, HlsError>(SegmentLoad {
                    cached,
                    meta: SegmentMeta {
                        variant,
                        segment_type: SegmentType::Media(segment_index),
                        sequence: segment.sequence,
                        url: segment_url.clone(),
                        duration: Some(segment.duration),
                        key: segment.key.clone(),
                        len: segment_len,
                        container,
                    },
                })
            })
            .await;
        self.clear_media_segment_cell(&fetch_key, &cell);

        let load = load?;
        Ok((load.meta.clone(), load.cached))
    }

    pub(crate) async fn load_media_segment_with_source(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<(SegmentMeta, bool)> {
        self.load_media_segment_with_source_for_epoch(variant, segment_index, 0)
            .await
    }

    /// Get Content-Length for a URL using HEAD request via the unified
    /// [`Downloader`]. Returns the size in bytes if Content-Length is
    /// present and parseable.
    ///
    /// # Errors
    /// Returns an error when the request fails, header is missing, or header value cannot be parsed.
    pub async fn get_content_length(&self, url: &Url) -> HlsResult<u64> {
        let cmd = FetchCmd {
            method: FetchMethod::Head,
            url: url.clone(),
            range: None,
            headers: self.headers.clone(),
            priority: Priority::Normal,
            on_connect: None,
            writer: None,
            on_complete: None,
            throttle: None,
        };
        let resp_headers = match self.downloader.execute(cmd).await {
            DlFetchResult::Ok { headers, .. } => headers,
            DlFetchResult::Err(e) => return Err(HlsError::from(e)),
        };
        let content_length = resp_headers
            .get("content-length")
            .or_else(|| resp_headers.get("Content-Length"))
            .ok_or_else(|| {
                HlsError::InvalidUrl(format!(
                    "No Content-Length header in HEAD response for {url}",
                ))
            })?;

        content_length.parse::<u64>().map_err(|e| {
            HlsError::InvalidUrl(format!(
                "Invalid Content-Length '{content_length}' for {url}: {e}",
            ))
        })
    }
}

/// Legacy alias kept for migration — `FetchManager` is no longer generic.
pub type DefaultFetchManager = FetchManager;

// Loader impl for FetchManager

impl Loader for FetchManager {
    async fn load_media_segment(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<SegmentMeta> {
        let (meta, _cached) = self
            .load_media_segment_with_source(variant, segment_index)
            .await?;
        Ok(meta)
    }

    async fn load_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        // Delegate to the OnceCell-based method on FetchManager
        Self::load_init_segment(self, variant).await
    }

    fn num_variants(&self) -> usize {
        self.cache.num_variants()
    }

    async fn num_segments(&self, variant: usize) -> HlsResult<usize> {
        if let Some(playlist_state) = self.playlist_state()
            && let Some(count) =
                crate::playlist::PlaylistAccess::num_segments(playlist_state.as_ref(), variant)
        {
            return Ok(count);
        }

        let (_media_url, playlist) = self.cache.load_media_playlist(variant).await?;
        Ok(playlist.segments.len())
    }
}
