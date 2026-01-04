use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::Stream;
use hls_m3u8::MediaPlaylist;
use kithara_assets::AssetCache;
use kithara_net::HttpClient;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext, abr::ThroughputSampleSource};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Cache(#[from] kithara_assets::CacheError),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Segment not found: {0}")]
    SegmentNotFound(String),

    #[error("Encryption error: {0}")]
    Encryption(String),
}

pub type SegmentStream<'a> = Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct FetchBytes {
    pub bytes: Bytes,
    pub source: ThroughputSampleSource,
    pub duration: Duration,
}

pub struct FetchManager {
    cache: AssetCache,
    net: HttpClient,
}

impl FetchManager {
    pub fn new(cache: AssetCache, net: HttpClient) -> Self {
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

        let fetch = self.fetch_resource(&segment_url, "segment.ts").await?;

        // Decrypt if needed
        if let Some(key_ctx) = key_context {
            self.decrypt_segment(fetch.bytes, key_ctx).await
        } else {
            Ok(fetch.bytes)
        }
    }

    pub async fn fetch_segment_with_meta(
        &self,
        media_playlist: &MediaPlaylist<'_>,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<FetchBytes> {
        let segment = media_playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::SegmentNotFound(format!("Index {}", segment_index)))?;

        let segment_url = base_url
            .join(&segment.uri())
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        let mut fetch = self.fetch_resource(&segment_url, "segment.ts").await?;
        if let Some(key_ctx) = key_context {
            fetch.bytes = self.decrypt_segment(fetch.bytes, key_ctx).await?;
        }

        Ok(fetch)
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        Ok(self.fetch_resource(init_url, "init.mp4").await?.bytes)
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
                    Ok(fetch) => {
                        // Decrypt if needed
                        let bytes = if let Some(key_ctx) = &key_ctx {
                            match fetcher.decrypt_segment(fetch.bytes, key_ctx).await {
                                Ok(decrypted) => decrypted,
                                Err(e) => {
                                    yield Err(e);
                                    continue;
                                }
                            }
                        } else {
                            fetch.bytes
                        };
                        yield Ok(bytes);
                    }
                    Err(e) => yield Err(e),
                }
            }
        })
    }

    async fn fetch_resource(&self, url: &Url, _default_filename: &str) -> HlsResult<FetchBytes> {
        // NOTE: Assets integration is being redesigned to use the new resource-based API
        // (`kithara-assets` + `kithara-storage`).
        //
        // The old cache layer (`kithara-cache`) supported `CachePath` + `put_atomic` and is no
        // longer available here. For now, fetch from the network only.
        let _ = &self.cache;

        let start = Instant::now();
        let bytes = self.net.get_bytes(url.clone(), None).await?;
        let duration = start.elapsed();

        Ok(FetchBytes {
            bytes,
            source: ThroughputSampleSource::Network,
            duration,
        })
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
}
