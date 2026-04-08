#![forbid(unsafe_code)]

//! Playlist fetch + parse + cache for HLS.
//!
//! Owns master/media playlist state, the disk cache for playlist bodies,
//! and URL resolution helpers. Network traffic routes through the
//! shared [`crate::atomic_fetch::fetch_atomic_body`] helper, which in
//! turn drives the unified [`Downloader`]. Clone-friendly — the
//! `OnceCell` state lives behind `Arc` so clones see the same cached
//! playlists.

use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::AssetStore;
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{RwLock, tokio::sync::OnceCell};
use kithara_stream::dl::Downloader;
use url::Url;

use crate::{
    HlsError, HlsResult,
    atomic_fetch::fetch_atomic_body,
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
/// the same master/media `OnceCell` buckets and configured headers/URLs.
#[derive(Clone)]
pub struct PlaylistCache {
    backend: AssetStore<DecryptContext>,
    downloader: Downloader,
    /// Cache-wide config (headers, master URL, base URL override). Held
    /// behind `Arc<RwLock<...>>` so clones see the same builder
    /// mutations.
    config: Arc<RwLock<PlaylistConfig>>,
    master: Arc<OnceCell<MasterPlaylist>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
    num_variants_cache: Arc<RwLock<Option<usize>>>,
}

#[derive(Default, Clone)]
struct PlaylistConfig {
    headers: Option<Headers>,
    master_url: Option<Url>,
    base_url: Option<Url>,
}

impl PlaylistCache {
    #[must_use]
    pub fn new(backend: AssetStore<DecryptContext>, downloader: Downloader) -> Self {
        Self {
            backend,
            downloader,
            config: Arc::new(RwLock::new(PlaylistConfig::default())),
            master: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
            num_variants_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_master_url(&self, url: Url) {
        self.config.lock_sync_write().master_url = Some(url);
    }

    pub fn set_base_url(&self, url: Option<Url>) {
        self.config.lock_sync_write().base_url = url;
    }

    pub fn set_headers(&self, headers: Option<Headers>) {
        self.config.lock_sync_write().headers = headers;
    }

    /// Builder-style override of the base URL — used by integration
    /// tests that want to verify URL resolution without going through
    /// the full `Hls::create` flow.
    #[must_use]
    pub fn with_base_url(self, url: Option<Url>) -> Self {
        self.set_base_url(url);
        self
    }

    #[must_use]
    pub fn headers(&self) -> Option<Headers> {
        self.config.lock_sync_read().headers.clone()
    }

    fn master_url_clone(&self) -> HlsResult<Url> {
        self.config
            .lock_sync_read()
            .master_url
            .clone()
            .ok_or_else(|| HlsError::InvalidUrl("master_url not set on PlaylistCache".to_string()))
    }

    /// Load and parse the master playlist. First call fetches from the
    /// network; subsequent calls return the cached parsed struct.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
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

    /// Load and parse the media playlist for `variant_id`. Deduplicated
    /// via a per-variant `OnceCell`.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
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

    #[must_use]
    pub fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    /// Variant count derived from the master playlist, cached after the
    /// first successful read.
    #[must_use]
    pub fn num_variants(&self) -> usize {
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
    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let base_override = self.config.lock_sync_read().base_url.clone();
        let resolved = if let Some(base_url) = base_override {
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
    pub async fn load_media_playlist(&self, variant: usize) -> HlsResult<(Url, MediaPlaylist)> {
        let master_url = self.master_url_clone()?;
        let master = self.master_playlist(&master_url).await?;

        let variant_stream = master
            .variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("variant {variant}")))?;

        let media_url = self.resolve_url(&master_url, &variant_stream.uri)?;
        let playlist = self.media_playlist(&media_url, VariantId(variant)).await?;

        Ok((media_url, playlist))
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let headers = self.headers();
        let bytes = fetch_atomic_body(
            &self.downloader,
            &self.backend,
            headers,
            url,
            basename,
            "playlist",
        )
        .await?;
        parse(&bytes)
    }
}
