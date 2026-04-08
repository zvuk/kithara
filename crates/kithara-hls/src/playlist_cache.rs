#![forbid(unsafe_code)]

//! Playlist fetch + parse + cache for HLS.
//!
//! Owns master/media playlist state, the disk cache for playlist bodies,
//! and URL resolution helpers. Every network fetch routes through the
//! unified [`Downloader`]. Shared between [`FetchManager`] and future
//! key/segment modules via [`Clone`] — the `OnceCell` state is held
//! behind `Arc` so clones see the same cached playlists.
//!
//! [`FetchManager`]: crate::fetch::FetchManager

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_bufpool::byte_pool;
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{RwLock, tokio::sync::OnceCell};
use kithara_storage::ResourceExt;
use kithara_stream::dl::{
    Downloader, FetchCmd, FetchMethod, FetchResult as DlFetchResult, Priority,
};
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError, HlsResult,
    parsing::{
        MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
        parse_media_playlist,
    },
};

/// Derive a safe basename from a URL path for disk cache storage.
fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

/// Playlist fetch + parse + disk cache.
///
/// Clone-friendly: all mutable state lives behind `Arc`, so clones see
/// the same master/media `OnceCell` buckets.
#[derive(Clone)]
pub(crate) struct PlaylistCache {
    backend: AssetStore<DecryptContext>,
    downloader: Downloader,
    headers: Option<Headers>,
    master_url: Option<Url>,
    base_url: Option<Url>,
    master: Arc<OnceCell<MasterPlaylist>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
    num_variants_cache: Arc<RwLock<Option<usize>>>,
}

impl PlaylistCache {
    pub(crate) fn new(backend: AssetStore<DecryptContext>, downloader: Downloader) -> Self {
        Self {
            backend,
            downloader,
            headers: None,
            master_url: None,
            base_url: None,
            master: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
            num_variants_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn set_master_url(&mut self, url: Url) {
        self.master_url = Some(url);
    }

    pub(crate) fn set_base_url(&mut self, url: Option<Url>) {
        self.base_url = url;
    }

    pub(crate) fn set_headers(&mut self, headers: Option<Headers>) {
        self.headers = headers;
    }

    pub(crate) fn headers(&self) -> Option<&Headers> {
        self.headers.as_ref()
    }

    pub(crate) fn master_url(&self) -> HlsResult<&Url> {
        self.master_url
            .as_ref()
            .ok_or_else(|| HlsError::InvalidUrl("master_url not set on PlaylistCache".to_string()))
    }

    /// Fetch an atomic body (playlist or DRM key) through the disk
    /// cache + unified downloader pipeline.
    ///
    /// Both the playlist and key paths share this helper: a best-effort
    /// resource lookup, a single `Downloader::execute` on cache miss,
    /// and a write-back to the asset store.
    ///
    /// # Errors
    /// Returns an error when cache access or the network fetch fails.
    pub(crate) async fn fetch_atomic_to_store(
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
                    asset_root = %self.backend.asset_root(),
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
            asset_root = %self.backend.asset_root(),
            rel_path = %rel_path,
            resource_kind,
            "kithara-hls: cache miss -> fetching from network"
        );

        let res = self.backend.acquire_resource(&key)?;
        let bytes = self.fetch_atomic_bytes(url.clone(), headers).await?;

        // Best-effort cache write. Concurrent callers may race here: all
        // miss the cache, all fetch from network, first commits the
        // resource, subsequent `write_all` calls fail because the
        // resource is already committed. Harmless — the bytes are in
        // memory from the network fetch.
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
                asset_root = %self.backend.asset_root(),
                rel_path = %rel_path,
                bytes = bytes.len(),
                resource_kind,
                "kithara-hls: fetched from network and cached"
            );
        }

        Ok(bytes)
    }

    /// Fetch a small body via `Downloader::execute` (GET, accumulating).
    async fn fetch_atomic_bytes(&self, url: Url, headers: Option<Headers>) -> HlsResult<Bytes> {
        let cmd = FetchCmd {
            method: FetchMethod::Get,
            url,
            range: None,
            headers,
            priority: Priority::High,
            on_connect: None,
            writer: None,
            on_complete: None,
            throttle: None,
        };
        match self.downloader.execute(cmd).await {
            DlFetchResult::Ok { body, .. } => Ok(body.map(Bytes::from).unwrap_or_default()),
            DlFetchResult::Err(e) => Err(HlsError::from(e)),
        }
    }

    /// Load and parse the master playlist. First call fetches from the
    /// network; subsequent calls return the cached parsed struct.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub(crate) async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let master = self
            .master
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "master_playlist", parse_master_playlist)
                    .await
            })
            .await?;
        Ok(master.clone())
    }

    /// Load and parse the media playlist for `variant_id`. Deduplicated
    /// via a per-variant `OnceCell`.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub(crate) async fn media_playlist(
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

    pub(crate) fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    /// Variant count derived from the master playlist, cached after the
    /// first successful read.
    pub(crate) fn num_variants(&self) -> usize {
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

    /// Resolve a possibly-relative target URL against `base`, honoring
    /// any override base URL configured on the cache.
    ///
    /// # Errors
    /// Returns an error when URL joining fails.
    pub(crate) fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
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

    /// Load the media playlist for `variant`, returning both its
    /// resolved URL and the parsed struct. Used by segment-loader paths
    /// that need both pieces.
    ///
    /// # Errors
    /// Returns an error when the master playlist, URL resolution, or
    /// media playlist fetch fails.
    pub(crate) async fn load_media_playlist(
        &self,
        variant: usize,
    ) -> HlsResult<(Url, MediaPlaylist)> {
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

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let bytes = self
            .fetch_atomic_to_store(url, basename, self.headers.clone(), "playlist")
            .await?;
        parse(&bytes)
    }
}
