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

pub type SegmentStream<'a> = Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + 'a>>;

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
        media_playlist: &MediaPlaylist<'_>,
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

    pub fn stream_segment_sequence(
        &self,
        media_playlist: MediaPlaylist<'_>,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> SegmentStream<'static> {
        let cache = self.cache.clone();
        let net = self.net.clone();
        let base_url = base_url.clone();
        let key_ctx = key_context.cloned();

        // Extract segment URIs upfront to avoid lifetime issues
        let segment_uris: Vec<String> = media_playlist
            .segments
            .iter()
            .map(|(_, segment)| segment.uri().to_string())
            .collect();

        Box::pin(async_stream::stream! {
            for segment_uri in segment_uris {
                let fetcher = FetchManager::new(cache.clone(), net.clone());
                let segment_url = match base_url.join(&segment_uri) {
                    Ok(url) => url,
                    Err(e) => {
                        yield Err(HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)));
                        continue;
                    }
                };

                match fetcher.fetch_resource(&segment_url, "segment.ts").await {
                    Ok(bytes) => {
                        // Decrypt if needed
                        let bytes = if let Some(key_ctx) = &key_ctx {
                            match fetcher.decrypt_segment(bytes, key_ctx).await {
                                Ok(decrypted) => decrypted,
                                Err(e) => {
                                    yield Err(e);
                                    continue;
                                }
                            }
                        } else {
                            bytes
                        };
                        yield Ok(bytes);
                    }
                    Err(e) => yield Err(e),
                }
            }
        })
    }

    async fn fetch_resource(&self, url: &Url, default_filename: &str) -> HlsResult<Bytes> {
        let asset_id = AssetId::from_url(url)?;
        let cache_path = self.cache_path_for_url(url, default_filename)?;
        let handle = self.cache.asset(asset_id);

        if handle.exists(&cache_path) {
            let mut file = handle.open(&cache_path)?.unwrap();

            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
            return Ok(bytes::Bytes::from(buf));
        }

        let bytes = self.net.get_bytes(url.clone()).await?;
        handle.put_atomic(&cache_path, &bytes)?;
        Ok(bytes)
    }

    async fn decrypt_segment(&self, bytes: Bytes, _key_context: &KeyContext) -> HlsResult<Bytes> {
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
            HlsError::from(kithara_cache::CacheError::InvalidPath(format!(
                "Invalid cache path: {}",
                e
            )))
        })
    }
}
