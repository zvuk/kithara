use hls_m3u8::{MasterPlaylist, MediaPlaylist};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_net::HttpClient;
use kithara_storage::Resource as _;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{HlsError, HlsResult};

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
        let bytes = self
            .fetch_playlist_atomic(url, "playlists/master.m3u8")
            .await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        // Convert to 'static lifetime by leaking the string
        let leaked: &'static str = Box::leak(content.into_boxed_str());
        let playlist = hls_m3u8::MasterPlaylist::try_from(leaked)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;
        Ok(playlist)
    }

    pub async fn fetch_media_playlist(&self, url: &Url) -> HlsResult<MediaPlaylist<'static>> {
        // Use the resolved URL string for a deterministic rel_path key.
        let key_name = format!("playlists/media/{}.m3u8", hex::encode(url.as_str()));
        let bytes = self.fetch_playlist_atomic(url, &key_name).await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        // Convert to 'static lifetime by leaking the string
        let leaked: &'static str = Box::leak(content.into_boxed_str());
        let playlist = hls_m3u8::MediaPlaylist::try_from(leaked)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;
        Ok(playlist)
    }

    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        // Use configured base_url override if available
        let resolved_base = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {}", e))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {}", e)))?
        };
        Ok(resolved_base)
    }

    async fn fetch_playlist_atomic(&self, url: &Url, rel_path: &str) -> HlsResult<bytes::Bytes> {
        // HLS playlists are metadata => AtomicResource.
        //
        // Asset layout contract (kithara-assets):
        // - one logical asset_root for the HLS session (provided by the session opener).
        let key = ResourceKey::new(self.asset_root.clone(), rel_path);

        let cancel = CancellationToken::new();
        let res = self.assets.open_atomic_resource(&key, cancel).await?;

        // Best-effort cache read.
        let cached = res.read().await?;
        if !cached.is_empty() {
            return Ok(cached);
        }

        // Cache miss => fetch and write atomically.
        let bytes = self.net.get_bytes(url.clone(), None).await?;
        res.write(&bytes).await?;
        Ok(bytes)
    }
}
