use hls_m3u8::{MasterPlaylist, MediaPlaylist};
use kithara_assets::AssetStore;
use kithara_net::HttpClient;
use thiserror::Error;
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
    assets: AssetStore,
    net: HttpClient,
    base_url: Option<Url>,
}

impl PlaylistManager {
    pub fn new(assets: AssetStore, net: HttpClient, base_url: Option<Url>) -> Self {
        Self {
            assets,
            net,
            base_url,
        }
    }

    pub async fn fetch_master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist<'static>> {
        let bytes = self.fetch_resource(url, "master.m3u8").await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        // Convert to 'static lifetime by leaking the string
        let leaked: &'static str = Box::leak(content.into_boxed_str());
        let playlist = hls_m3u8::MasterPlaylist::try_from(leaked)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;
        Ok(playlist)
    }

    pub async fn fetch_media_playlist(&self, url: &Url) -> HlsResult<MediaPlaylist<'static>> {
        let bytes = self.fetch_resource(url, "media.m3u8").await?;
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

    async fn fetch_resource(&self, url: &Url, _default_filename: &str) -> HlsResult<bytes::Bytes> {
        // NOTE: Assets integration is being redesigned to use the new resource-based API
        // (`kithara-assets` + `kithara-storage`).
        //
        // The old cache layer (`kithara-cache`) supported `CachePath` + `put_atomic` and is no
        // longer available here. For now, fetch from the network only.
        let _ = &self.assets;
        let bytes = self.net.get_bytes(url.clone(), None).await?;
        Ok(bytes)
    }
}
