#![forbid(unsafe_code)]

use std::sync::Arc;

use dashmap::DashMap;
use kithara_assets::AssetScope;
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{sync::RwLock, tokio::sync::OnceCell};
use kithara_stream::dl::PeerHandle;
use url::Url;

use super::parse::{
    MediaPlaylist, ParsedMaster, VariantId, parse_master_playlist, parse_media_playlist,
};
use crate::{HlsError, HlsResult, handle::PlaylistPeer};

/// Playlist fetch + parse + disk cache.
///
/// Clone-friendly: all mutable state lives behind `Arc`, so clones see
/// the same master/media `OnceCell` buckets and configured headers/URLs.
#[derive(Clone)]
pub struct PlaylistCache {
    /// Cache-wide config (headers, master URL, base URL override). Held
    /// behind `Arc<RwLock<...>>` so clones see the same builder
    /// mutations.
    config: Arc<RwLock<PlaylistConfig>>,
    master: Arc<OnceCell<ParsedMaster>>,
    /// Per-variant media-playlist `OnceCell` buckets. `DashMap`
    /// fine-grained locks keep parallel variants out of each other's
    /// critical sections.
    media: Arc<DashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>,
    /// Cache-first + downloader pipeline for `.m3u8` playlist bodies.
    fetch: PlaylistPeer,
}

#[derive(Default, Clone)]
struct PlaylistConfig {
    base_url: Option<Url>,
    headers: Option<Headers>,
    master_url: Option<Url>,
}

impl PlaylistCache {
    #[must_use]
    pub fn new(
        scope: AssetScope<DecryptContext>,
        downloader: PeerHandle,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        Self {
            fetch: PlaylistPeer::new(downloader, scope, byte_pool),
            config: Arc::new(RwLock::default()),
            master: Arc::new(OnceCell::default()),
            media: Arc::new(DashMap::new()),
        }
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let headers = self.headers();
        let bytes = self.fetch.fetch(url, headers).await?;
        parse(&bytes)
    }

    #[must_use]
    pub fn headers(&self) -> Option<Headers> {
        self.config.read().headers.clone()
    }

    /// Load and parse the master playlist. First call fetches from the
    /// network; subsequent calls return the cached parsed struct.
    ///
    /// # Errors
    /// Returns an error when fetching or parsing fails.
    pub async fn master_playlist(&self, url: &Url) -> HlsResult<ParsedMaster> {
        let master = self
            .master
            .get_or_try_init(|| async { self.fetch_and_parse(url, parse_master_playlist).await })
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
        let cell: Arc<OnceCell<MediaPlaylist>> = self
            .media
            .entry(variant_id)
            .or_insert_with(|| Arc::new(OnceCell::default()))
            .clone();

        let playlist = cell
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, |bytes| parse_media_playlist(url.clone(), bytes))
                    .await
            })
            .await?;
        Ok(playlist.clone())
    }

    /// Resolve a possibly-relative target URL against `base`, honoring
    /// any override base URL configured on the cache.
    ///
    /// # Errors
    /// Returns an error when URL joining fails.
    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let base_override = self.config.read().base_url.clone();
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

    pub fn set_base_url(&self, url: Option<Url>) {
        self.config.write().base_url = url;
    }

    pub fn set_headers(&self, headers: Option<Headers>) {
        self.config.write().headers = headers;
    }

    pub fn set_master_url(&self, url: Url) {
        self.config.write().master_url = Some(url);
    }
}
