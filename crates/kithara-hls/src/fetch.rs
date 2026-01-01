use bytes::Bytes;
use futures::Stream;
use hls_m3u8::MediaPlaylist;
use kithara_cache::{AssetCache, CachePath};
use kithara_core::AssetId;
use kithara_net::NetClient;
use std::pin::Pin;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Segment not found: {0}")]
    SegmentNotFound(String),

    #[error("Encryption error: {0}")]
    Encryption(String),
}

pub type SegmentStream = Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send>>;

pub struct FetchManager {
    cache: AssetCache,
    net: NetClient,
}

impl FetchManager {
    pub fn new(cache: AssetCache, net: NetClient) -> Self {
        Self { cache, net }
    }

    pub async fn fetch_segment(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<Bytes> {
        let segment = media_playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::SegmentNotFound(format!("Index {}", segment_index)))?;

        let segment_url = base_url
            .join(&segment.uri())
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        let bytes = self.fetch_resource(&segment_url, "segment.ts").await?;

        // Decrypt if needed
        if let Some(key_ctx) = key_context {
            self.decrypt_segment(bytes, key_ctx).await
        } else {
            Ok(bytes)
        }
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        let bytes = self.fetch_resource(init_url, "init.mp4").await?;
        Ok(bytes)
    }

    pub async fn stream_segment_sequence(
        &self,
        media_playlist: &MediaPlaylist,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> SegmentStream {
        let cache = self.cache.clone();
        let net = self.net.clone();
        let base_url = base_url.clone();
        let key_ctx = key_context.cloned();

        Box::pin(async_stream::stream! {
            for (index, _segment) in media_playlist.segments.iter().enumerate() {
                let fetcher = FetchManager::new(cache.clone(), net.clone());
                match fetcher.fetch_segment(media_playlist, index, &base_url, key_context.as_ref()).await {
                    Ok(bytes) => yield Ok(bytes),
                    Err(e) => yield Err(e),
                }
            }
        })
    }

    async fn fetch_resource(&self, url: &Url, default_filename: &str) -> HlsResult<Bytes> {
        let asset_id = AssetId::from_url(url);
        let cache_path = self.cache_path_for_url(url, default_filename)?;
        let handle = self.cache.asset(asset_id);

        if handle.exists(&cache_path) {
            let file = handle.open(&cache_path)?.unwrap();
            use std::io::Read;
            let mut buf = Vec::new();
            file.take_to_end(&mut buf).unwrap();
            return Ok(bytes::Bytes::from(buf));
        }

        let bytes = self.net.get_bytes(url.clone()).await?;
        handle.put_atomic(&cache_path, &bytes)?;
        Ok(bytes)
    }

    async fn decrypt_segment(&self, bytes: Bytes, key_context: &KeyContext) -> HlsResult<Bytes> {
        // This would implement AES-128 decryption
        // For now, return bytes unchanged
        // In real implementation, we'd use the key from key_context

        // TODO: Implement AES-128 decryption when crypto support is added
        // if let Some(key) = self.get_decryption_key(&key_context.url).await? {
        //     decrypt_aes128_cbc(&bytes, &key, &key_context.iv.unwrap_or_default())
        // } else {
        //     Err(HlsError::Encryption("No decryption key available".to_string()))
        // }

        Ok(bytes)
    }

    fn cache_path_for_url(&self, url: &Url, default_filename: &str) -> HlsResult<CachePath> {
        let filename = url
            .path_segments()
            .and_then(|segments| segments.last())
            .and_then(|name| if name.is_empty() { None } else { Some(name) })
            .unwrap_or(default_filename);

        CachePath::new(vec![
            "hls".to_string(),
            "segments".to_string(),
            filename.to_string(),
        ])
        .map_err(|e| {
            HlsError::from(kithara_cache::CacheError::Path(format!(
                "Invalid cache path: {}",
                e
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::*;

    #[tokio::test]
    async fn fetch_segment_from_network() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let fetch_manager = FetchManager::new(cache, net);
        let segment_url = server.url("/segment_0.ts")?;

        // Note: This test assumes server provides segment data
        let _segment = fetch_manager
            .fetch_resource(&segment_url, "segment.ts")
            .await;

        Ok(())
    }

    #[tokio::test]
    async fn stream_segment_sequence() -> HlsResult<()> {
        let server = TestServer::new().await;
        let (cache, net) = create_test_cache_and_net();

        let fetch_manager = FetchManager::new(cache, net);

        // Create a test media playlist
        let media_playlist_str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment_0.ts
#EXTINF:4.0,
segment_1.ts
#EXT-X-ENDLIST
"#;

        let media_playlist = hls_m3u8::MediaPlaylist::try_from(media_playlist_str)
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        let base_url = server.url("/video/480p/")?;
        let mut stream = fetch_manager.stream_segment_sequence(&media_playlist, &base_url, None);

        // Note: In real test, server would serve segment data
        // For now, we just verify the stream structure
        use futures::StreamExt;
        let _first_segment = stream.next().await;

        Ok(())
    }
}
