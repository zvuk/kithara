#![forbid(unsafe_code)]

//! Fetch layer: network fetch + disk cache + playlist management + segment loading.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetResource, AssetResourceState, AssetStore, ResourceKey};
use kithara_bufpool::byte_pool;
use kithara_drm::DecryptContext;
use kithara_net::{Headers, HttpClient, Net};
use kithara_platform::{MaybeSend, MaybeSync, RwLock, tokio, tokio::sync::OnceCell};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{ContainerFormat, Writer, WriterItem};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError, HlsResult,
    ids::{SegmentIndex, VariantIndex},
    parsing::{
        EncryptionMethod, MasterPlaylist, MediaPlaylist, SegmentKey, VariantId, VariantStream,
        parse_master_playlist, parse_media_playlist,
    },
    playlist::PlaylistState,
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
pub enum FetchResult {
    /// Already cached, no fetch needed.
    Cached { bytes: u64 },
    /// Active fetch in progress via Writer stream.
    /// Includes the resource so the caller can commit after download completes.
    Active(Writer, Box<AssetResource<DecryptContext>>),
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

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

/// Unified HLS fetch manager.
///
/// Handles low-level network fetch + disk cache, playlist parsing/caching,
/// and implements `Loader` for segment loading.
#[derive(Clone)]
pub struct FetchManager<N> {
    backend: AssetStore<DecryptContext>,
    key_manager: Option<Arc<crate::keys::KeyManager>>,
    net: N,
    cancel: CancellationToken,
    headers: Option<Headers>,
    // Playlist state
    master_url: Option<Url>,
    base_url: Option<Url>,
    master: Arc<OnceCell<MasterPlaylist>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
    num_variants_cache: Arc<RwLock<Option<usize>>>,
    // Init segment deduplication: first caller downloads, others wait on OnceCell
    init_segments: Arc<RwLock<HashMap<usize, Arc<OnceCell<SegmentMeta>>>>>,
    // Media segment deduplication for active network fetches only.
    media_segments: Arc<RwLock<HashMap<SegmentFetchKey, Arc<OnceCell<SegmentLoad>>>>>,
    // Parsed playlist state (populated in Hls::create after loading all media playlists)
    playlist_state: Option<Arc<PlaylistState>>,
}

impl<N: Net> FetchManager<N> {
    pub fn new(backend: AssetStore<DecryptContext>, net: N, cancel: CancellationToken) -> Self {
        Self {
            backend,
            key_manager: None,
            net,
            cancel,
            headers: None,
            master_url: None,
            base_url: None,
            master: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
            num_variants_cache: Arc::new(RwLock::new(None)),
            init_segments: Arc::new(RwLock::new(HashMap::new())),
            media_segments: Arc::new(RwLock::new(HashMap::new())),
            playlist_state: None,
        }
    }

    /// Set master playlist URL (required for Loader functionality).
    pub fn with_master_url(mut self, url: Url) -> Self {
        self.master_url = Some(url);
        self
    }

    /// Set base URL override for resolving relative URLs.
    pub fn with_base_url(mut self, url: Option<Url>) -> Self {
        self.base_url = url;
        self
    }

    /// Set key manager for DRM decryption.
    pub fn with_key_manager(mut self, km: Arc<crate::keys::KeyManager>) -> Self {
        self.key_manager = Some(km);
        self
    }

    /// Set additional HTTP headers for all requests.
    #[must_use]
    pub fn with_headers(mut self, headers: Option<Headers>) -> Self {
        self.headers = headers;
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
    pub fn playlist_state(&self) -> Option<&Arc<PlaylistState>> {
        self.playlist_state.as_ref()
    }

    /// Set the parsed playlist state.
    pub fn set_playlist_state(&mut self, state: Arc<PlaylistState>) {
        self.playlist_state = Some(state);
    }

    pub fn asset_root(&self) -> &str {
        self.backend.asset_root()
    }

    pub fn backend(&self) -> &AssetStore<DecryptContext> {
        &self.backend
    }

    // Low-level fetch

    /// Fetch a playlist-like resource and cache it in the assets backend.
    ///
    /// # Errors
    /// Returns an error when cache access, network fetch, or URL/resource handling fails.
    pub async fn fetch_playlist(&self, url: &Url, rel_path: &str) -> HlsResult<Bytes> {
        self.fetch_atomic_internal(url, rel_path, self.headers.clone(), "playlist")
            .await
    }

    /// Fetch a key resource and cache it in the assets backend.
    ///
    /// # Errors
    /// Returns an error when cache access, network fetch, or URL/resource handling fails.
    pub async fn fetch_key(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        let merged = self.merge_headers(headers);
        self.fetch_atomic_internal(url, rel_path, merged, "key")
            .await
    }

    async fn fetch_atomic_internal(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
        resource_kind: &str,
    ) -> HlsResult<Bytes> {
        let key = ResourceKey::from_url(url);
        if let Ok(res) = self.backend.open_resource(&key) {
            let mut buf = byte_pool().get();
            let n = res.read_into(&mut buf)?;
            if n > 0 {
                debug!(
                    url = %url,
                    asset_root = %self.asset_root(),
                    rel_path = %rel_path,
                    bytes = n,
                    resource_kind,
                    "kithara-hls: cache hit"
                );
                return Ok(Bytes::copy_from_slice(&buf));
            }
        }

        debug!(
            url = %url,
            asset_root = %self.asset_root(),
            rel_path = %rel_path,
            resource_kind,
            "kithara-hls: cache miss -> fetching from network"
        );

        let res = self.backend.acquire_resource(&key)?;
        let bytes = self.net.get_bytes(url.clone(), headers).await?;

        // Best-effort cache write. Multiple concurrent callers may race here:
        // all miss the cache, all fetch from network, first commits the resource,
        // subsequent write_all calls fail because the resource is already committed.
        // This is harmless — the bytes are in memory from the network fetch.
        if let Err(e) = res.write_all(&bytes) {
            trace!(
                url = %url,
                error = %e,
                resource_kind,
                "kithara-hls: cache write failed (concurrent commit), using network bytes"
            );
        } else {
            debug!(
                url = %url,
                asset_root = %self.asset_root(),
                rel_path = %rel_path,
                bytes = bytes.len(),
                resource_kind,
                "kithara-hls: fetched from network and cached"
            );
        }

        Ok(bytes)
    }

    /// Start fetching a segment. Returns cached size if already cached.
    ///
    /// When `decrypt_ctx` is `Some`, the resource will be decrypted on commit
    /// via the `ProcessingAssets` layer (zero-allocation, pool-backed).
    pub(crate) async fn start_fetch(
        &self,
        url: &Url,
        decrypt_ctx: Option<DecryptContext>,
    ) -> HlsResult<FetchResult> {
        let key = ResourceKey::from_url(url);
        if let AssetResourceState::Committed { final_len } = self.backend.resource_state(&key)? {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit via resource_state");
            return Ok(FetchResult::Cached { bytes: len });
        }

        let res = self.backend.acquire_resource_with_ctx(&key, decrypt_ctx)?;

        let status = res.status();
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit");
            return Ok(FetchResult::Cached { bytes: len });
        }

        trace!(url = %url, "start_fetch: starting network fetch");
        let net_stream = self.net.stream(url.clone(), self.headers.clone()).await?;
        let writer = Writer::new(net_stream, res.clone(), self.cancel.clone());

        Ok(FetchResult::Active(writer, Box::new(res)))
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

    // Playlist management

    /// Load and parse the master playlist.
    ///
    /// # Errors
    /// Returns an error when fetching/parsing fails or the URL basename cannot be derived.
    pub async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let master = self
            .master
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "master_playlist", parse_master_playlist)
                    .await
            })
            .await?;

        Ok(master.clone())
    }

    /// Load and parse the media playlist for a specific variant.
    ///
    /// # Errors
    /// Returns an error when fetching/parsing fails or the URL basename cannot be derived.
    pub async fn media_playlist(
        &self,
        url: &Url,
        variant_id: VariantId,
    ) -> HlsResult<MediaPlaylist> {
        let cell = {
            let mut guard = self.media.lock_sync_write();
            guard
                .entry(variant_id)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let playlist = cell
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "media_playlist", |bytes| {
                    parse_media_playlist(bytes, variant_id)
                })
                .await
            })
            .await?;

        Ok(playlist.clone())
    }

    pub fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    /// Resolve a possibly relative target URL.
    ///
    /// # Errors
    /// Returns an error when URL joining fails.
    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let resolved = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {e}"))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {e}")))?
        };

        Ok(resolved)
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let bytes = self.fetch_playlist(url, basename).await?;
        parse(&bytes)
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
        let (media_url, playlist) = self.load_media_playlist(variant).await?;
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

        let fetch_result = self.start_fetch(&init_url, decrypt_ctx).await?;

        let init_len = match fetch_result {
            FetchResult::Cached { bytes } => {
                trace!(variant, bytes, "init segment already cached");
                bytes
            }
            FetchResult::Active(mut writer, res) => {
                debug!(variant, url = %init_url, "downloading init segment");
                let mut total = 0u64;
                while let Some(result) = writer.next().await {
                    match result {
                        Ok(WriterItem::ChunkWritten { len, .. }) => total += len as u64,
                        Ok(WriterItem::StreamEnded { total_bytes }) => {
                            total = total_bytes;
                            break;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                if total == 0 {
                    res.fail("init segment: 0 bytes downloaded".to_string());
                    return Err(HlsError::SegmentNotFound(
                        "init segment download yielded 0 bytes".to_string(),
                    ));
                }
                res.commit(Some(total)).map_err(HlsError::from)?;
                let committed_len = res.len().unwrap_or(total);
                debug!(variant, total, committed_len, "init segment downloaded");
                committed_len
            }
        };

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
    /// Pattern matches `media_playlist()`.
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
        let (media_url, playlist) = self.load_media_playlist(variant).await?;

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
                let fetch_result = self.start_fetch(&segment_url, decrypt_ctx).await?;

                let (segment_len, cached) = match fetch_result {
                    FetchResult::Cached { bytes } => (bytes, true),
                    FetchResult::Active(mut writer, res) => {
                        let mut total = 0u64;
                        while let Some(result) = writer.next().await {
                            match result {
                                Ok(WriterItem::ChunkWritten { len, .. }) => {
                                    total += len as u64;
                                }
                                Ok(WriterItem::StreamEnded { total_bytes }) => {
                                    total = total_bytes;
                                    break;
                                }
                                Err(e) => return Err(e.into()),
                            }
                        }
                        if total == 0 {
                            res.fail("segment: 0 bytes downloaded".to_string());
                            return Err(HlsError::SegmentNotFound(format!(
                                "segment {segment_index} download yielded 0 bytes",
                            )));
                        }
                        let committed_len = {
                            res.commit(Some(total)).map_err(HlsError::from)?;
                            res.len().unwrap_or(total)
                        };
                        (committed_len, false)
                    }
                };

                Ok(SegmentLoad {
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

    // Loader helpers

    fn master_url(&self) -> HlsResult<&Url> {
        self.master_url
            .as_ref()
            .ok_or_else(|| HlsError::InvalidUrl("master_url not set on FetchManager".to_string()))
    }

    async fn load_media_playlist(&self, variant: usize) -> HlsResult<(Url, MediaPlaylist)> {
        let master_url = self.master_url()?;
        let master = self.master_playlist(master_url).await?;

        let variant_stream = master
            .variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("variant {variant}")))?;

        let media_url = self.resolve_url(master_url, &variant_stream.uri)?;
        let playlist = self.media_playlist(&media_url, VariantId(variant)).await?;

        Ok((media_url, playlist))
    }

    /// Get Content-Length for a URL using HEAD request.
    ///
    /// Returns the size in bytes if Content-Length header is present and parseable.
    ///
    /// # Errors
    /// Returns an error when the request fails, header is missing, or header value cannot be parsed.
    pub async fn get_content_length(&self, url: &Url) -> HlsResult<u64> {
        let resp = self.net.head(url.clone(), self.headers.clone()).await?;
        let content_length = resp
            .get("content-length")
            .or_else(|| resp.get("Content-Length"))
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

pub type DefaultFetchManager = FetchManager<HttpClient>;

// Loader impl for FetchManager

impl<N: Net> Loader for FetchManager<N> {
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
        if let Some(cached) = *self.num_variants_cache.lock_sync_read() {
            return cached;
        }

        if let Some(variants) = self.master_variants() {
            let count = variants.len();
            *self.num_variants_cache.lock_sync_write() = Some(count);
            return count;
        }

        0
    }

    async fn num_segments(&self, variant: usize) -> HlsResult<usize> {
        if let Some(playlist_state) = self.playlist_state()
            && let Some(count) =
                crate::playlist::PlaylistAccess::num_segments(playlist_state.as_ref(), variant)
        {
            return Ok(count);
        }

        let (_media_url, playlist) = self.load_media_playlist(variant).await?;
        Ok(playlist.segments.len())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{
        collections::HashMap,
        num::NonZeroUsize,
        path::Path,
        sync::{
            Arc, Mutex as StdMutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use aes::Aes128;
    use bytes::Bytes;
    use cbc::{
        Encryptor,
        cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
    };
    use futures::stream;
    use kithara_assets::{AssetStore, AssetStoreBuilder, ProcessChunkFn};
    use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
    use kithara_net::{ByteStream, Headers, NetError, mock::NetMock};
    use kithara_test_utils::kithara;
    use tempfile::TempDir;
    use tokio::{task::yield_now as task_yield_now, time::sleep as tokio_sleep};
    use tokio_util::sync::CancellationToken;
    use unimock::{MockFn, Unimock, matching};
    use url::Url;

    use super::*;

    /// Build a test backend with DRM process function.
    fn test_backend(asset_root: &str, root_dir: &Path) -> AssetStore<DecryptContext> {
        let drm_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        AssetStoreBuilder::new()
            .process_fn(drm_fn)
            .asset_root(Some(asset_root))
            .root_dir(root_dir)
            .build()
    }

    fn test_backend_ephemeral(asset_root: &str) -> AssetStore<DecryptContext> {
        let drm_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        AssetStoreBuilder::new()
            .process_fn(drm_fn)
            .asset_root(Some(asset_root))
            .ephemeral(true)
            .cache_capacity(NonZeroUsize::new(2).expect("non-zero"))
            .build()
    }

    fn encrypt_aes128_cbc(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
        let padded_len = plaintext.len() + (16 - plaintext.len() % 16);
        let mut buf = vec![0u8; padded_len];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let ct = encryptor
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .expect("encrypt_padded_mut failed");
        ct.to_vec()
    }

    const MASTER_SINGLE_VARIANT_PLAYLIST: &str = "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
         v0.m3u8\n";
    const V0_SINGLE_SEGMENT_PLAYLIST: &str = "#EXTM3U\n\
         #EXT-X-TARGETDURATION:4\n\
         #EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXTINF:4.0,\n\
         seg0.ts\n\
         #EXT-X-ENDLIST\n";

    static SEGMENT_STREAM_CALLS: AtomicUsize = AtomicUsize::new(0);
    static SEGMENT_STREAM_TEST_LOCK: StdMutex<()> = StdMutex::new(());
    static DRM_STREAMS: OnceLock<StdMutex<HashMap<String, Vec<Bytes>>>> = OnceLock::new();

    type StreamAnswer =
        dyn Fn(&Unimock, Url, Option<Headers>) -> Result<ByteStream, NetError> + Send + Sync;

    fn drm_streams() -> &'static StdMutex<HashMap<String, Vec<Bytes>>> {
        DRM_STREAMS.get_or_init(|| StdMutex::new(HashMap::new()))
    }

    #[derive(Clone, Copy)]
    enum FetchKind {
        Key,
        Playlist,
    }

    fn test_master_url() -> Url {
        Url::parse("http://test.com/master.m3u8").expect("valid master URL")
    }

    fn test_url(raw: &str) -> Url {
        Url::parse(raw).expect("valid test URL")
    }

    fn test_fetch_manager_with_get_bytes_response(
        response: Result<Bytes, NetError>,
    ) -> (TempDir, FetchManager<Unimock>) {
        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(response),
        );
        test_fetch_manager(mock_net)
    }

    fn test_fetch_manager(mock_net: Unimock) -> (TempDir, FetchManager<Unimock>) {
        let temp_dir = TempDir::new().unwrap();
        let backend = test_backend("test", temp_dir.path());
        let fetch = FetchManager::new(backend, mock_net, CancellationToken::new());
        (temp_dir, fetch)
    }

    fn partial_fetch_manager(
        stream_answer: &'static StreamAnswer,
    ) -> (TempDir, FetchManager<Unimock>) {
        let temp_dir = TempDir::new().unwrap();
        let backend = test_backend("partial-test", temp_dir.path());
        let mock_net = Unimock::new((
            NetMock::get_bytes.stub(|each| {
                each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        MASTER_SINGLE_VARIANT_PLAYLIST.as_bytes(),
                    )));
                each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        V0_SINGLE_SEGMENT_PLAYLIST.as_bytes(),
                    )));
            }),
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(stream_answer),
        ));
        let fetch = FetchManager::new(backend, mock_net, CancellationToken::new())
            .with_master_url(test_master_url());
        (temp_dir, fetch)
    }

    fn ephemeral_fetch_manager(mock_net: Unimock) -> FetchManager<Unimock> {
        let backend = test_backend_ephemeral("ephemeral-test");
        FetchManager::new(backend, mock_net, CancellationToken::new())
            .with_master_url(test_master_url())
    }

    fn counting_segment_stream(
        _mock: &Unimock,
        url: Url,
        _headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        SEGMENT_STREAM_CALLS.fetch_add(1, Ordering::SeqCst);
        let stream = stream::once(async move {
            task_yield_now().await;
            Ok(Bytes::from(format!("payload:{url}")))
        });
        Ok(ByteStream::without_headers(Box::pin(stream)))
    }

    fn slow_counting_segment_stream(
        _mock: &Unimock,
        url: Url,
        _headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        SEGMENT_STREAM_CALLS.fetch_add(1, Ordering::SeqCst);
        let stream = stream::once(async move {
            tokio_sleep(Duration::from_millis(25)).await;
            Ok(Bytes::from(format!("payload:{url}")))
        });
        Ok(ByteStream::without_headers(Box::pin(stream)))
    }

    fn drm_segment_stream(
        _mock: &Unimock,
        url: Url,
        _headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        if url.host_str().is_none() {
            return Err(NetError::Unimplemented);
        }
        let chunks = drm_streams()
            .lock()
            .expect("DRM stream map lock")
            .get(url.path())
            .cloned()
            .expect("DRM stream chunks must be initialized before stream()")
            .clone()
            .into_iter()
            .map(Ok);
        Ok(ByteStream::without_headers(Box::pin(stream::iter(chunks))))
    }

    // FetchManager tests

    #[kithara::test(tokio)]
    #[case(FetchKind::Playlist)]
    #[case(FetchKind::Key)]
    async fn test_fetch_with_mock_net(#[case] kind: FetchKind) {
        let (url, rel_path, expected) = match kind {
            FetchKind::Playlist => (
                test_url("http://example.com/playlist.m3u8"),
                "playlist.m3u8",
                Bytes::from_static(b"#EXTM3U\n#EXT-X-VERSION:6\n"),
            ),
            FetchKind::Key => (
                test_url("http://example.com/key.bin"),
                "key.bin",
                Bytes::from_static(&[0u8; 16]),
            ),
        };
        let (_temp_dir, fetch_manager) =
            test_fetch_manager_with_get_bytes_response(Ok(expected.clone()));
        let result: HlsResult<Bytes> = match kind {
            FetchKind::Playlist => fetch_manager.fetch_playlist(&url, rel_path).await,
            FetchKind::Key => fetch_manager.fetch_key(&url, rel_path, None).await,
        };

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected);
    }

    #[kithara::test(tokio)]
    async fn test_fetch_playlist_uses_cache() {
        let url = test_url("http://example.com/playlist.m3u8");
        let playlist_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";

        let (_temp_dir, fetch_manager) =
            test_fetch_manager_with_get_bytes_response(Ok(Bytes::from_static(playlist_content)));

        let result1: HlsResult<Bytes> = fetch_manager.fetch_playlist(&url, "playlist.m3u8").await;
        assert!(result1.is_ok());

        let result2: HlsResult<Bytes> = fetch_manager.fetch_playlist(&url, "playlist.m3u8").await;
        assert!(result2.is_ok());
        assert_eq!(result1.unwrap(), result2.unwrap());
    }

    #[kithara::test(tokio)]
    #[case(FetchKind::Playlist)]
    #[case(FetchKind::Key)]
    async fn test_fetch_with_network_error(#[case] kind: FetchKind) {
        let (url, rel_path) = match kind {
            FetchKind::Playlist => (
                test_url("http://example.com/playlist.m3u8"),
                "playlist.m3u8",
            ),
            FetchKind::Key => (test_url("http://example.com/key.bin"), "key.bin"),
        };
        let (_temp_dir, fetch_manager) =
            test_fetch_manager_with_get_bytes_response(Err(NetError::Timeout));
        let result: HlsResult<Bytes> = match kind {
            FetchKind::Playlist => fetch_manager.fetch_playlist(&url, rel_path).await,
            FetchKind::Key => fetch_manager.fetch_key(&url, rel_path, None).await,
        };

        assert!(result.is_err());
    }

    #[kithara::test(tokio)]
    async fn test_matcher_url_path_matching() {
        let master_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";
        let media_content = b"#EXTINF:4.0\nseg0.bin\n";

        let mock_net = Unimock::new(NetMock::get_bytes.stub(|each| {
            each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                .returns(Ok(Bytes::from_static(master_content)));
            each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                .returns(Ok(Bytes::from_static(media_content)));
        }));
        let (_temp_dir, fetch_manager) = test_fetch_manager(mock_net);

        let master_url = test_url("http://example.com/master.m3u8");
        let master: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&master_url, "master.m3u8")
            .await;
        assert!(master.is_ok());

        let media_url = test_url("http://example.com/v0.m3u8");
        let media: HlsResult<Bytes> = fetch_manager.fetch_playlist(&media_url, "v0.m3u8").await;
        assert!(media.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_matcher_url_domain_matching() {
        let content = b"test content";

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!((url, _) if url.host_str() == Some("cdn1.example.com")))
                .returns(Ok(Bytes::from_static(content))),
        );
        let (_temp_dir, fetch_manager) = test_fetch_manager(mock_net);

        let url = test_url("http://cdn1.example.com/file.m3u8");
        let result: HlsResult<Bytes> = fetch_manager.fetch_playlist(&url, "file.m3u8").await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_matcher_headers_matching() {
        let key_content = vec![0u8; 16];

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!((_, headers) if headers.as_ref().is_some_and(|h| h.get("Authorization").is_some())))
                .returns(Ok(Bytes::from(key_content))),
        );
        let (_temp_dir, fetch_manager) = test_fetch_manager(mock_net);

        let url = test_url("http://example.com/key.bin");
        let mut headers_map = HashMap::new();
        headers_map.insert("Authorization".to_string(), "Bearer token".to_string());
        let headers = Some(Headers::from(headers_map));

        let result: HlsResult<Bytes> = fetch_manager.fetch_key(&url, "key.bin", headers).await;

        assert!(result.is_ok());
    }

    // Loader tests

    fn create_test_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
        SegmentMeta {
            variant,
            segment_type: SegmentType::Media(segment_index),
            sequence: segment_index as u64,
            url: Url::parse(&format!(
                "http://test.com/v{}/seg{}.ts",
                variant, segment_index
            ))
            .expect("valid URL"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(ContainerFormat::MpegTs),
        }
    }

    #[kithara::test(tokio)]
    async fn test_mock_loader_basic() {
        let loader = Unimock::new((
            LoaderMock::num_variants
                .some_call(matching!())
                .returns(3_usize),
            LoaderMock::load_media_segment
                .some_call(matching!(0, 5))
                .answers(&|_, variant, idx| Ok(create_test_meta(variant, idx, 200_000))),
            LoaderMock::num_segments
                .some_call(matching!(0))
                .returns(Ok(100_usize)),
        ));

        assert_eq!(loader.num_variants(), 3);
        assert_eq!(loader.num_segments(0).await.unwrap(), 100);

        let meta = loader.load_media_segment(0, 5).await.unwrap();
        assert_eq!(meta.variant, 0);
        assert_eq!(meta.segment_type.media_index(), Some(5));
        assert_eq!(meta.len, 200_000);
    }

    #[kithara::test(tokio)]
    async fn test_mock_loader_multi_variant() {
        let loader = Unimock::new(
            LoaderMock::load_media_segment
                .each_call(matching!(_, _))
                .answers(&|_, variant, idx| {
                    Ok(create_test_meta(
                        variant,
                        idx,
                        200_000 + variant as u64 * 50_000,
                    ))
                }),
        );

        let meta0 = loader.load_media_segment(0, 1).await.unwrap();
        let meta1 = loader.load_media_segment(1, 1).await.unwrap();
        let meta2 = loader.load_media_segment(2, 1).await.unwrap();

        assert_eq!(meta0.len, 200_000);
        assert_eq!(meta1.len, 250_000);
        assert_eq!(meta2.len, 300_000);
    }

    // Partial segment tests

    #[expect(
        clippy::needless_pass_by_value,
        reason = "signature must match NetMock::stream callback shape in tests"
    )]
    fn stream_with_timeout(
        _mock: &Unimock,
        url: Url,
        _headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        if url.host_str().is_none() {
            return Err(NetError::Unimplemented);
        }
        let stream = stream::iter(vec![
            Ok(Bytes::from(vec![0xFF; 1000])),
            Err(NetError::Timeout),
        ]);
        Ok(ByteStream::without_headers(Box::pin(stream)))
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "signature must match NetMock::stream callback shape in tests"
    )]
    fn empty_stream(
        _mock: &Unimock,
        url: Url,
        _headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        if url.host_str().is_none() {
            return Err(NetError::Unimplemented);
        }
        let stream = stream::empty::<Result<Bytes, NetError>>();
        Ok(ByteStream::without_headers(Box::pin(stream)))
    }

    #[kithara::test(tokio)]
    #[case(&stream_with_timeout)]
    #[case(&empty_stream)]
    async fn test_load_media_segment_invalid_stream_returns_err(
        #[case] stream_answer: &'static StreamAnswer,
    ) {
        let (_temp_dir, fetch) = partial_fetch_manager(stream_answer);
        let result = fetch.load_media_segment(0, 0).await;
        assert!(result.is_err());
    }

    #[kithara::test(tokio)]
    async fn concurrent_reload_of_evicted_media_segment_shares_single_network_fetch() {
        let _guard = SEGMENT_STREAM_TEST_LOCK
            .lock()
            .expect("segment stream test lock");
        SEGMENT_STREAM_CALLS.store(0, Ordering::SeqCst);
        let mock_net = Unimock::new((
            NetMock::get_bytes.stub(|each| {
                each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        MASTER_SINGLE_VARIANT_PLAYLIST.as_bytes(),
                    )));
                each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        V0_SINGLE_SEGMENT_PLAYLIST.as_bytes(),
                    )));
            }),
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(&slow_counting_segment_stream),
        ));

        let fetch = ephemeral_fetch_manager(mock_net);
        let seg0 = test_url("http://test.com/seg0.ts");

        let (meta, cached) = fetch.load_media_segment_with_source(0, 0).await.unwrap();
        assert!(!cached);
        assert_eq!(meta.url, seg0);
        assert_eq!(SEGMENT_STREAM_CALLS.load(Ordering::SeqCst), 1);

        for i in 0..16 {
            let key = ResourceKey::new(format!("evict-{i}.m4s"));
            let res = fetch.backend().acquire_resource(&key).unwrap();
            res.write_at(0, b"x").unwrap();
            res.commit(Some(1)).unwrap();
        }

        assert!(
            matches!(
                fetch
                    .backend()
                    .resource_state(&ResourceKey::from_url(&seg0))
                    .unwrap(),
                AssetResourceState::Missing
            ),
            "ephemeral resource must be evicted before re-download"
        );

        let (left, right) = tokio::join!(
            fetch.load_media_segment_with_source(0, 0),
            fetch.load_media_segment_with_source(0, 0)
        );

        assert!(left.is_ok(), "left={left:?}");
        assert!(right.is_ok(), "right={right:?}");
        assert_eq!(
            SEGMENT_STREAM_CALLS.load(Ordering::SeqCst),
            2,
            "concurrent re-download must issue exactly one additional network fetch"
        );
    }

    #[kithara::test(tokio)]
    async fn concurrent_reload_across_seek_epochs_shares_single_network_fetch() {
        let _guard = SEGMENT_STREAM_TEST_LOCK
            .lock()
            .expect("segment stream test lock");
        SEGMENT_STREAM_CALLS.store(0, Ordering::SeqCst);
        let mock_net = Unimock::new((
            NetMock::get_bytes.stub(|each| {
                each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        MASTER_SINGLE_VARIANT_PLAYLIST.as_bytes(),
                    )));
                each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                    .returns(Ok(Bytes::from_static(
                        V0_SINGLE_SEGMENT_PLAYLIST.as_bytes(),
                    )));
            }),
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(&counting_segment_stream),
        ));

        let fetch = ephemeral_fetch_manager(mock_net);
        let seg0 = test_url("http://test.com/seg0.ts");

        let (meta, cached) = fetch.load_media_segment_with_source(0, 0).await.unwrap();
        assert!(!cached);
        assert_eq!(meta.url, seg0);
        assert_eq!(SEGMENT_STREAM_CALLS.load(Ordering::SeqCst), 1);

        for i in 0..16 {
            let key = ResourceKey::new(format!("evict-{i}.m4s"));
            let res = fetch.backend().acquire_resource(&key).unwrap();
            res.write_at(0, b"x").unwrap();
            res.commit(Some(1)).unwrap();
        }

        assert!(
            matches!(
                fetch
                    .backend()
                    .resource_state(&ResourceKey::from_url(&seg0))
                    .unwrap(),
                AssetResourceState::Missing
            ),
            "ephemeral resource must be evicted before re-download"
        );

        let (left, right) = tokio::join!(
            fetch.load_media_segment_with_source_for_epoch(0, 0, 1),
            fetch.load_media_segment_with_source_for_epoch(0, 0, 2)
        );

        assert!(left.is_ok(), "left={left:?}");
        assert!(right.is_ok(), "right={right:?}");
        assert_eq!(
            SEGMENT_STREAM_CALLS.load(Ordering::SeqCst),
            2,
            "cross-epoch re-download must still issue exactly one additional network fetch"
        );
    }

    #[kithara::test(tokio)]
    async fn start_fetch_disk_drm_reopens_committed_bytes_after_cache_eviction() {
        let _guard = SEGMENT_STREAM_TEST_LOCK
            .lock()
            .expect("segment stream test lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let drm_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        let backend = AssetStoreBuilder::new()
            .process_fn(drm_fn)
            .asset_root(Some("disk-drm-reopen"))
            .root_dir(temp_dir.path())
            .cache_capacity(NonZeroUsize::new(1).expect("non-zero"))
            .build();

        let key = [0x41u8; 16];
        let iv = [0x17u8; 16];
        let plaintext: Vec<u8> = (0u8..=u8::MAX).cycle().take(512 * 1024 + 37).collect();
        let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);
        drm_streams().lock().expect("DRM stream map lock").insert(
            "/seg0.ts".to_string(),
            vec![
                Bytes::copy_from_slice(&ciphertext[..128 * 1024]),
                Bytes::copy_from_slice(&ciphertext[128 * 1024..320 * 1024]),
                Bytes::copy_from_slice(&ciphertext[320 * 1024..]),
            ],
        );
        let mock_net = Unimock::new(
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(&drm_segment_stream),
        );
        let fetch = FetchManager::new(backend, mock_net, CancellationToken::new());
        let seg0 = test_url("http://test.com/seg0.ts");

        let (total, res) = match fetch
            .start_fetch(&seg0, Some(DecryptContext::new(key, iv)))
            .await
            .expect("start fetch")
        {
            FetchResult::Active(mut writer, res) => {
                let mut total = 0u64;
                while let Some(item) = writer.next().await {
                    match item.expect("writer item") {
                        WriterItem::ChunkWritten { len, .. } => total += len as u64,
                        WriterItem::StreamEnded { total_bytes } => {
                            total = total_bytes;
                            break;
                        }
                    }
                }
                (total, res)
            }
            FetchResult::Cached { .. } => panic!("expected active fetch"),
        };
        res.commit(Some(total)).expect("commit segment");

        let other_plaintext = b"other-segment";
        let other_ciphertext = encrypt_aes128_cbc(other_plaintext, &key, &iv);
        let other_key = ResourceKey::new("other.m4s");
        let other = fetch
            .backend()
            .acquire_resource_with_ctx(&other_key, Some(DecryptContext::new(key, iv)))
            .expect("acquire other resource");
        other.write_at(0, &other_ciphertext).expect("write other");
        other
            .commit(Some(other_ciphertext.len() as u64))
            .expect("commit other");

        let reopened = fetch
            .backend()
            .open_resource(&ResourceKey::from_url(&seg0))
            .expect("reopen resource");
        let mut buf = vec![0u8; plaintext.len()];
        let read = reopened.read_at(0, &mut buf).expect("read reopened");

        assert_eq!(read, plaintext.len());
        assert_eq!(buf, plaintext);
    }

    #[kithara::test(tokio)]
    async fn start_fetch_disk_drm_reopens_multiple_committed_segments_after_eviction() {
        let _guard = SEGMENT_STREAM_TEST_LOCK
            .lock()
            .expect("segment stream test lock");
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let drm_fn: ProcessChunkFn<DecryptContext> =
            Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
                aes128_cbc_process_chunk(input, output, ctx, is_last)
            });
        let backend = AssetStoreBuilder::new()
            .process_fn(drm_fn)
            .asset_root(Some("disk-drm-multi"))
            .root_dir(temp_dir.path())
            .cache_capacity(NonZeroUsize::new(1).expect("non-zero"))
            .build();
        let fetch = FetchManager::new(
            backend,
            Unimock::new(
                NetMock::stream
                    .some_call(matching!((url, _) if url.path().starts_with("/seg")))
                    .answers(&drm_segment_stream),
            ),
            CancellationToken::new(),
        );

        let key = [0x41u8; 16];
        let mut expected = Vec::new();
        {
            let mut streams = drm_streams().lock().expect("DRM stream map lock");
            streams.clear();
            for index in 0..4 {
                let iv = [0x17u8.wrapping_add(index as u8); 16];
                let plaintext: Vec<u8> = (0u8..=u8::MAX)
                    .cycle()
                    .skip(index * 17)
                    .take(256 * 1024 + 37 + index * 1024)
                    .collect();
                let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);
                streams.insert(
                    format!("/seg{index}.ts"),
                    vec![
                        Bytes::copy_from_slice(&ciphertext[..64 * 1024]),
                        Bytes::copy_from_slice(&ciphertext[64 * 1024..160 * 1024]),
                        Bytes::copy_from_slice(&ciphertext[160 * 1024..]),
                    ],
                );
                expected.push((
                    test_url(&format!("http://test.com/seg{index}.ts")),
                    plaintext,
                    iv,
                ));
            }
        }

        for (url, plaintext, iv) in &expected {
            let (total, res) = match fetch
                .start_fetch(url, Some(DecryptContext::new(key, *iv)))
                .await
                .expect("start fetch")
            {
                FetchResult::Active(mut writer, res) => {
                    let mut total = 0u64;
                    while let Some(item) = writer.next().await {
                        match item.expect("writer item") {
                            WriterItem::ChunkWritten { len, .. } => total += len as u64,
                            WriterItem::StreamEnded { total_bytes } => {
                                total = total_bytes;
                                break;
                            }
                        }
                    }
                    (total, res)
                }
                FetchResult::Cached { .. } => panic!("expected active fetch"),
            };
            res.commit(Some(total)).expect("commit segment");
            assert!(
                total as usize >= plaintext.len(),
                "encrypted bytes must cover plaintext + padding"
            );
        }

        for (url, plaintext, _) in &expected {
            let reopened = fetch
                .backend()
                .open_resource(&ResourceKey::from_url(url))
                .expect("reopen resource");
            let mut buf = vec![0u8; plaintext.len()];
            let read = reopened.read_at(0, &mut buf).expect("read reopened");
            assert_eq!(read, plaintext.len(), "url={url}");
            assert_eq!(buf, *plaintext, "url={url}");
        }
    }
}
