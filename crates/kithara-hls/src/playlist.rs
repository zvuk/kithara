use hls_m3u8::{MasterPlaylist, MediaPlaylist};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_net::HttpClient;
use kithara_storage::Resource as _;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{HlsError, HlsResult};

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Playlist parsing error: {0}")]
    Parse(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

pub struct PlaylistManager {
    asset_root: String,
    assets: AssetStore,
    net: HttpClient,
    base_url: Option<Url>,
}

impl PlaylistManager {
    pub fn new(
        asset_root: String,
        assets: AssetStore,
        net: HttpClient,
        base_url: Option<Url>,
    ) -> Self {
        Self {
            asset_root,
            assets,
            net,
            base_url,
        }
    }

    pub async fn fetch_master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist<'static>> {
        debug!(url = %url, "kithara-hls: fetch_master_playlist begin");

        let basename = uri_basename_no_query(url.as_str()).ok_or_else(|| {
            HlsError::InvalidUrl("Failed to derive master playlist basename".into())
        })?;
        let bytes = self.fetch_playlist_atomic(url, basename).await?;
        debug!(
            url = %url,
            bytes = bytes.len(),
            "kithara-hls: fetch_master_playlist got bytes"
        );

        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        // Convert to 'static lifetime by leaking the string
        let leaked: &'static str = Box::leak(content.into_boxed_str());
        let playlist = hls_m3u8::MasterPlaylist::try_from(leaked)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        debug!(
            url = %url,
            variants = playlist.variant_streams.len(),
            "kithara-hls: fetch_master_playlist parsed"
        );

        Ok(playlist)
    }

    pub async fn fetch_media_playlist(&self, url: &Url) -> HlsResult<MediaPlaylist<'static>> {
        debug!(url = %url, "kithara-hls: fetch_media_playlist begin");

        let basename = uri_basename_no_query(url.as_str()).ok_or_else(|| {
            HlsError::InvalidUrl("Failed to derive media playlist basename".into())
        })?;
        let bytes = self.fetch_playlist_atomic(url, basename).await?;
        debug!(
            url = %url,
            bytes = bytes.len(),
            "kithara-hls: fetch_media_playlist got bytes"
        );

        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        // Convert to 'static lifetime by leaking the string
        let leaked: &'static str = Box::leak(content.into_boxed_str());
        let playlist = hls_m3u8::MediaPlaylist::try_from(leaked)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        debug!(
            url = %url,
            segments = playlist.segments.iter().count(),
            "kithara-hls: fetch_media_playlist parsed"
        );

        Ok(playlist)
    }

    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        trace!(
            base = %base,
            target = %target,
            base_override = self.base_url.as_ref().map(|u| u.as_str()),
            "kithara-hls: resolve_url begin"
        );

        // Use configured base_url override if available
        let resolved = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {}", e))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {}", e)))?
        };

        trace!(resolved = %resolved, "kithara-hls: resolve_url done");
        Ok(resolved)
    }

    async fn fetch_playlist_atomic(&self, url: &Url, rel_path: &str) -> HlsResult<bytes::Bytes> {
        // HLS playlists are metadata => AtomicResource.
        //
        // Disk layout contract (stream-download-hls compatible, adapted to kithara-assets):
        // - asset_root = "<master_hash>"
        // - rel_path   = "<playlist_basename>"
        //
        // The basename is derived from the URL and ignores query string.
        let key = ResourceKey::new(self.asset_root.clone(), rel_path);

        debug!(
            url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            "kithara-hls: playlist fetch (atomic) begin"
        );

        let cancel = CancellationToken::new();
        let res = self.assets.open_atomic_resource(&key, cancel).await?;

        // Best-effort cache read.
        let cached = res.read().await?;
        if !cached.is_empty() {
            debug!(
                url = %url,
                asset_root = %self.asset_root,
                rel_path = %rel_path,
                bytes = cached.len(),
                "kithara-hls: playlist cache hit"
            );
            return Ok(cached);
        }

        debug!(
            url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            "kithara-hls: playlist cache miss -> fetching from network"
        );

        // Cache miss => fetch and write atomically.
        let bytes = self.net.get_bytes(url.clone(), None).await?;
        res.write(&bytes).await?;

        debug!(
            url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            bytes = bytes.len(),
            "kithara-hls: playlist fetched from network and cached"
        );

        Ok(bytes)
    }
}
