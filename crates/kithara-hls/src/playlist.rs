use hls_m3u8::{MasterPlaylist, MediaPlaylist};
use kithara_cache::{AssetCache, CachePath};
use kithara_core::AssetId;
use kithara_net::NetClient;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult};

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),

    #[error("Playlist parsing error: {0}")]
    Parse(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

pub struct PlaylistManager {
    cache: AssetCache,
    net: NetClient,
    base_url: Option<Url>,
}

impl PlaylistManager {
    pub fn new(cache: AssetCache, net: NetClient, base_url: Option<Url>) -> Self {
        Self {
            cache,
            net,
            base_url,
        }
    }

    pub async fn fetch_master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let bytes = self.fetch_resource(url, "master.m3u8").await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        hls_m3u8::MasterPlaylist::try_from(content.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))
    }

    pub async fn fetch_media_playlist(&self, url: &Url) -> HlsResult<MediaPlaylist> {
        let bytes = self.fetch_resource(url, "media.m3u8").await?;
        let content = String::from_utf8(bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;

        hls_m3u8::MediaPlaylist::try_from(content.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))
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

    async fn fetch_resource(&self, url: &Url, default_filename: &str) -> HlsResult<bytes::Bytes> {
        let asset_id = AssetId::from_url(url)?;
        let cache_path = self.cache_path_for_url(url, default_filename)?;
        let handle = self.cache.asset(asset_id);

        if handle.exists(&cache_path) {
            let file = handle.open(&cache_path)?.unwrap();
            use std::io::Read;
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
            return Ok(bytes::Bytes::from(buf));
        }

        let bytes = self.net.get_bytes(url.clone()).await?;
        handle.put_atomic(&cache_path, &bytes)?;
        Ok(bytes)
    }

    fn cache_path_for_url(&self, url: &Url, default_filename: &str) -> HlsResult<CachePath> {
        let filename = url
            .path_segments()
            .and_then(|segments| segments.last())
            .and_then(|name| if name.is_empty() { None } else { Some(name) })
            .unwrap_or(default_filename);

        CachePath::new(vec!["hls".to_string(), filename.to_string()]).map_err(|e| {
            HlsError::from(kithara_cache::CacheError::InvalidPath(format!(
                "Invalid cache path: {}",
                e
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_master_playlist_from_network() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let playlist_manager = PlaylistManager::new(cache, net, None);
        let master_url = server.url("/master.m3u8")?;

        let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

        assert_eq!(master_playlist.variant_streams.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn fetch_media_playlist_from_network() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let playlist_manager = PlaylistManager::new(cache, net, None);
        let media_url = server.url("/video/480p/playlist.m3u8")?;

        let media_playlist = playlist_manager.fetch_media_playlist(&media_url).await?;

        let segment_count = media_playlist.segments.len();
        assert_eq!(segment_count, 3);
        Ok(())
    }

    #[tokio::test]
    async fn resolve_url_with_base_override() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let base_url = server.url("/custom/base/")?;
        let playlist_manager = PlaylistManager::new(cache, net, Some(base_url));

        let base = server.url("/master.m3u8")?;
        let resolved = playlist_manager.resolve_url(&base, "video/480p/playlist.m3u8")?;

        assert_eq!(resolved.as_str(), "/custom/base/video/480p/playlist.m3u8");
        Ok(())
    }
}
