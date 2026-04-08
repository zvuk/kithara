#![forbid(unsafe_code)]

//! Thin façade over the extracted HLS sub-systems.
//!
//! Holds a [`PlaylistCache`] for playlist fetch + parse + dedup, a
//! [`SegmentLoader`] for init/media segment downloads, and the parsed
//! [`PlaylistState`] once it has been built. All network traffic
//! flows through the unified [`Downloader`] (sole `HttpClient` owner
//! in production).

use std::sync::Arc;

use kithara_assets::AssetStore;
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_platform::{MaybeSend, MaybeSync};
use kithara_stream::dl::Downloader;
use tokio_util::sync::CancellationToken;
use url::Url;

// Public re-exports for backward compatibility with existing
// `crate::fetch::SegmentMeta` / `SegmentType` / `FetchResult` paths.
pub use crate::segment_loader::{FetchResult, SegmentMeta, SegmentType};
use crate::{
    HlsResult,
    ids::{SegmentIndex, VariantIndex},
    parsing::{MasterPlaylist, MediaPlaylist, VariantId},
    playlist::PlaylistState,
    playlist_cache::PlaylistCache,
    segment_loader::SegmentLoader,
};

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

// FetchManager façade

/// Thin façade over the extracted HLS sub-systems.
///
/// Holds a [`PlaylistCache`] + [`SegmentLoader`] and forwards calls
/// to them. After Phase 4a.5 this type will be removed in favour of
/// passing the smaller types directly to consumers.
#[derive(Clone)]
pub struct FetchManager {
    backend: AssetStore<DecryptContext>,
    #[expect(
        dead_code,
        reason = "used by future phase 3 dedicated-thread migration"
    )]
    cancel: CancellationToken,
    /// Playlist fetch + parse + disk cache. Shared via `Clone`.
    cache: PlaylistCache,
    /// Segment download + dedup + DRM context resolution.
    loader: SegmentLoader,
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
        let loader = SegmentLoader::new(downloader, backend.clone(), None, cache.clone());
        Self {
            backend,
            cancel,
            cache,
            loader,
            playlist_state: None,
        }
    }

    /// Set master playlist URL (required for Loader functionality).
    #[must_use]
    pub fn with_master_url(self, url: Url) -> Self {
        self.cache.set_master_url(url);
        self
    }

    /// Set base URL override for resolving relative URLs.
    #[must_use]
    pub fn with_base_url(self, url: Option<Url>) -> Self {
        self.cache.set_base_url(url);
        self
    }

    /// Set key manager for DRM decryption.
    #[must_use]
    pub fn with_key_manager(mut self, km: Arc<crate::keys::KeyManager>) -> Self {
        self.loader.set_key_manager(km);
        self
    }

    /// Set additional HTTP headers for all requests.
    #[must_use]
    pub fn with_headers(mut self, headers: Option<Headers>) -> Self {
        self.cache.set_headers(headers.clone());
        self.loader.set_headers(headers);
        self
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

    // Segment delegates

    /// Load init segment with deduplication via `OnceCell`.
    ///
    /// # Errors
    /// Returns an error when playlist loading, URL resolution, or
    /// fetch fails.
    pub async fn load_init_segment(&self, variant: usize) -> HlsResult<SegmentMeta> {
        self.loader.load_init_segment(variant).await
    }

    pub(crate) async fn load_media_segment_with_source_for_epoch(
        &self,
        variant: usize,
        segment_index: usize,
        seek_epoch: u64,
    ) -> HlsResult<(SegmentMeta, bool)> {
        self.loader
            .load_media_segment_with_source_for_epoch(variant, segment_index, seek_epoch)
            .await
    }

    pub(crate) async fn load_media_segment_with_source(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<(SegmentMeta, bool)> {
        self.loader
            .load_media_segment_with_source(variant, segment_index)
            .await
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
