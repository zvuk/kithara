#![forbid(unsafe_code)]

//! Segment download + dedup + DRM context resolution.
//!
//! Owns the disk cache + downloader handles directly. Pulls media
//! playlist info from a shared [`PlaylistCache`] and resolves DRM
//! decryption contexts via an optional [`KeyManager`]. No dependency
//! on `FetchManager` — all the segment loading state lives here.

use std::{io::Error as IoError, sync::Arc, time::Duration};

use dashmap::DashMap;
use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::tokio::sync::OnceCell;
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::dl::{FetchCmd, PeerHandle};
use tracing::{debug, trace};
use url::Url;

use super::{keys::KeyManager, playlist_cache::PlaylistCache};
use crate::{
    HlsError, HlsResult,
    parsing::{EncryptionMethod, SegmentKey},
};

/// AES-128 key length in bytes.
const AES_KEY_LEN: usize = 16;

// Public segment-data types

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub url: Url,
    pub duration: Option<Duration>,
    pub len: u64,
}

// SegmentLoader

/// HLS segment loader.
///
/// Owns init/media segment dedup state, the disk cache, the
/// downloader handle, and an optional key manager for DRM. Reads
/// media playlists through a shared [`PlaylistCache`].
#[derive(Clone)]
pub struct SegmentLoader {
    downloader: PeerHandle,
    backend: AssetStore<DecryptContext>,
    headers: Option<Headers>,
    cache: PlaylistCache,
    key_manager: Option<Arc<KeyManager>>,
    // Init segment deduplication: first caller downloads, others wait
    // on OnceCell. `DashMap` provides fine-grained locking so parallel
    // variants do not serialize on a single `RwLock`.
    init_segments: Arc<DashMap<usize, Arc<OnceCell<SegmentMeta>>>>,
}

impl SegmentLoader {
    #[must_use]
    pub fn new(
        downloader: PeerHandle,
        backend: AssetStore<DecryptContext>,
        headers: Option<Headers>,
        cache: PlaylistCache,
    ) -> Self {
        Self {
            downloader,
            backend,
            headers,
            cache,
            key_manager: None,
            init_segments: Arc::new(DashMap::new()),
        }
    }

    pub fn set_key_manager(&mut self, km: Arc<KeyManager>) {
        self.key_manager = Some(km);
    }

    /// Start fetching a segment via the unified [`PeerHandle`].
    ///
    /// Returns `(bytes, was_cached)`:
    /// - `was_cached == true` — resource was already committed in the
    ///   disk cache; no network round-trip occurred.
    /// - `was_cached == false` — fetch went through
    ///   `PeerHandle::execute` and the response body stream was
    ///   written into the `AssetResource` chunk by chunk.
    ///
    /// When `decrypt_ctx` is `Some`, the resource is opened with a
    /// decrypt context; decryption happens on the fly inside
    /// `AssetResource::write_at` via the `ProcessingAssets` layer.
    ///
    /// # Errors
    /// Returns an error when the cache lookup or network fetch fails.
    pub async fn start_fetch(
        &self,
        url: &Url,
        decrypt_ctx: Option<DecryptContext>,
    ) -> HlsResult<(u64, bool)> {
        let key = ResourceKey::from_url(url);
        if let Some(len) = self.backend.final_len(&key) {
            trace!(url = %url, len, "start_fetch: cache hit via AssetStore availability index");
            return Ok((len, true));
        }

        let res = self.backend.acquire_resource_with_ctx(&key, decrypt_ctx)?;

        let status = res.status();
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit");
            return Ok((len, true));
        }

        trace!(url = %url, "start_fetch: downloading via PeerHandle");
        let bytes = self.download_stream_via_downloader(url, &res).await?;
        Ok((bytes, false))
    }

    /// Stream a fetch into an `AssetResource` via [`PeerHandle`].
    async fn download_stream_via_downloader(
        &self,
        url: &Url,
        res: &AssetResource<DecryptContext>,
    ) -> HlsResult<u64> {
        let cmd = FetchCmd::get(url.clone()).headers(self.headers.clone());
        let resp = self.downloader.execute(cmd).await.map_err(|e| {
            res.fail(format!("fetch failed: {e}"));
            HlsError::from(e)
        })?;
        let res_writer = res.clone();
        let mut offset: u64 = 0;
        let total = resp
            .body
            .write_all(move |chunk| {
                let pos = offset;
                offset += chunk.len() as u64;
                res_writer.write_at(pos, chunk).map_err(IoError::other)
            })
            .await
            .map_err(|e| {
                res.fail(format!("body stream failed: {e}"));
                HlsError::from(e)
            })?;
        if total == 0 {
            res.fail(format!("0 bytes downloaded: {url}"));
            return Err(HlsError::SegmentNotFound(format!(
                "download yielded 0 bytes for {url}",
            )));
        }
        res.commit(Some(total)).map_err(HlsError::from)?;
        Ok(res.len().unwrap_or(total))
    }

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

        let iv = KeyManager::derive_iv(key_info, sequence);
        let key_url = KeyManager::resolve_key_url(key_info, segment_url)?;
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

    /// Download init segment for a variant. No race recovery needed —
    /// `OnceCell` guarantees exactly one caller performs the download.
    async fn fetch_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        let (media_url, playlist) = self.cache.load_media_playlist(variant).await?;

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

        let (init_len, _was_cached) = self.start_fetch(&init_url, decrypt_ctx).await?;

        // Pin init segment so the ephemeral LRU never evicts it.
        let init_key = ResourceKey::from_url(&init_url);
        if let Ok(res) = self.backend.open_resource(&init_key) {
            let _pinned = res.retain();
        }

        Ok(SegmentMeta {
            url: init_url,
            duration: None,
            len: init_len,
        })
    }

    /// Load init segment with deduplication via `OnceCell`.
    ///
    /// First caller downloads, concurrent callers wait on the same cell.
    ///
    /// # Errors
    /// Returns an error when playlist loading, URL resolution, fetch, or content-length detection fails.
    pub async fn load_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        let mut cell: Arc<OnceCell<SegmentMeta>> = self
            .init_segments
            .entry(variant)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        // Ephemeral: cached init metadata may outlive the actual resource
        // (evicted from LRU). Verify and re-fetch if needed.
        if self.backend.is_ephemeral()
            && let Some(meta) = cell.get()
            && !self.backend.has_resource(&ResourceKey::from_url(&meta.url))
        {
            debug!(variant, url = %meta.url, "init resource evicted, resetting cache");
            let new_cell = Arc::new(OnceCell::new());
            self.init_segments.insert(variant, Arc::clone(&new_cell));
            cell = new_cell;
        }

        let meta = cell
            .get_or_try_init(|| self.fetch_init_segment(variant))
            .await?;

        Ok(meta.clone())
    }

    /// Synchronous DRM context resolution using pre-fetched keys.
    ///
    /// Reads key bytes from [`AssetStore`] (disk cache). Returns `None`
    /// for unencrypted segments.
    fn resolve_decrypt_context_sync(
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

        let iv = KeyManager::derive_iv(key_info, sequence);
        let key_url = KeyManager::resolve_key_url(key_info, segment_url)?;
        let raw_key = km.get_cached_key(&key_url)?;

        if raw_key.len() != AES_KEY_LEN {
            return Err(HlsError::KeyProcessing(format!(
                "invalid AES-128 key length: {}",
                raw_key.len()
            )));
        }

        let mut key_bytes = [0u8; AES_KEY_LEN];
        key_bytes.copy_from_slice(&raw_key[..AES_KEY_LEN]);
        Ok(Some(DecryptContext::new(key_bytes, iv)))
    }

    /// Synchronous media segment preparation using pre-fetched data.
    ///
    /// All lookups (playlist, DRM keys) hit pre-populated caches.
    /// Called from `poll_next` — must not perform any async I/O.
    ///
    /// # Errors
    /// Returns an error when the playlist or DRM key lookup fails.
    pub fn prepare_media_sync(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<PreparedMedia> {
        let (media_url, playlist) =
            self.cache.get_media_playlist_sync(variant).ok_or_else(|| {
                HlsError::VariantNotFound(format!("variant {variant} playlist not pre-loaded",))
            })?;

        let segment = playlist.segments.get(segment_index).ok_or_else(|| {
            HlsError::SegmentNotFound(format!(
                "segment {segment_index} not found in variant {variant} playlist",
            ))
        })?;

        let segment_url = media_url
            .join(&segment.uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {e}")))?;

        let key = ResourceKey::from_url(&segment_url);
        if let Some(len) = self.backend.final_len(&key) {
            return Ok(PreparedMedia {
                url: segment_url,
                duration: Some(segment.duration),
                cached_len: Some(len),
                resource: None,
            });
        }

        let decrypt_ctx = self.resolve_decrypt_context_sync(
            segment.key.as_ref(),
            &segment_url,
            segment.sequence,
        )?;

        let res = self.backend.acquire_resource_with_ctx(&key, decrypt_ctx)?;

        let status = res.status();
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            return Ok(PreparedMedia {
                url: segment_url,
                duration: Some(segment.duration),
                cached_len: Some(len),
                resource: None,
            });
        }

        tracing::trace!(
            url = %segment_url,
            ?status,
            "prepare_media_sync: resource not cached, will download"
        );

        Ok(PreparedMedia {
            url: segment_url,
            duration: Some(segment.duration),
            cached_len: None,
            resource: Some(res),
        })
    }

    /// Read init segment metadata from cache (no network I/O).
    ///
    /// Returns `None` if the init segment hasn't been pre-fetched yet
    /// or if the resource was evicted from the ephemeral LRU.
    #[must_use]
    pub fn get_init_segment_cached(&self, variant: usize) -> Option<SegmentMeta> {
        let meta = self.init_segments.get(&variant)?.get()?.clone();
        if self.backend.is_ephemeral()
            && !self.backend.has_resource(&ResourceKey::from_url(&meta.url))
        {
            return None;
        }
        Some(meta)
    }

    /// Read init segment bytes from the asset store into `buf`.
    ///
    /// Returns number of bytes read, or `None` if the init segment
    /// isn't cached or can't be read.
    pub fn read_init_bytes(&self, variant: usize, buf: &mut Vec<u8>) -> Option<usize> {
        let meta = self.get_init_segment_cached(variant)?;
        let key = ResourceKey::from_url(&meta.url);
        let resource = self.backend.open_resource(&key).ok()?;
        buf.clear();
        let n = resource.read_into(buf).ok()?;
        if n == 0 {
            return None;
        }
        Some(n)
    }

    /// Complete a media segment after body has been written by the
    /// Downloader (via `on_chunk`).
    ///
    /// Commits the resource and returns segment metadata.
    ///
    /// # Errors
    /// Returns an error when commit fails or zero bytes were written.
    pub fn complete_media(
        &self,
        prepared: &PreparedMedia,
        bytes_written: u64,
    ) -> HlsResult<SegmentMeta> {
        if bytes_written == 0 && prepared.cached_len.is_none() {
            return Err(HlsError::SegmentNotFound(format!(
                "0 bytes downloaded for {}",
                prepared.url,
            )));
        }

        let len = if let Some(cached) = prepared.cached_len {
            cached
        } else if let Some(ref res) = prepared.resource {
            res.commit(Some(bytes_written)).map_err(HlsError::from)?;
            res.len().unwrap_or(bytes_written)
        } else {
            bytes_written
        };

        Ok(SegmentMeta {
            url: prepared.url.clone(),
            duration: prepared.duration,
            len,
        })
    }
}

/// A media segment prepared for download via `poll_next` / `on_chunk`.
///
/// If `cached_len` is `Some`, the segment is already in the disk cache
/// and no fetch is needed. Otherwise, `resource` holds the open
/// `AssetResource` for the Downloader to write into via `on_chunk`.
pub struct PreparedMedia {
    /// Segment URL.
    pub url: Url,
    /// Segment duration from the playlist.
    pub duration: Option<Duration>,
    /// Set when cached — no fetch needed.
    pub cached_len: Option<u64>,
    /// Open resource for writing. `None` when cached.
    pub resource: Option<AssetResource<DecryptContext>>,
}
