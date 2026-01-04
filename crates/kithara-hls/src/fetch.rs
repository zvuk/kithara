use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use hls_m3u8::MediaPlaylist;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_net::HttpClient;
use kithara_storage::{Resource as _, StreamingResourceExt};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext, abr::ThroughputSampleSource};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] kithara_storage::StorageError),

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
    asset_root: String,
    assets: AssetStore,
    net: HttpClient,
}

impl FetchManager {
    pub fn new(asset_root: String, assets: AssetStore, net: HttpClient) -> Self {
        Self {
            asset_root,
            assets,
            net,
        }
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
        let asset_root = self.asset_root.clone();
        let assets = self.assets.clone();
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
                let fetcher = FetchManager::new(asset_root.clone(), assets.clone(), net.clone());
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
        // HLS segments are large objects => StreamingResource.
        //
        // Contract:
        // - writer fills the resource via `write_at`,
        // - reader waits via `wait_range` (no "false EOF"),
        // - completion is signaled via `commit(Some(final_len))`,
        // - failures signal via `fail(...)`.
        //
        let rel_path = format!("segments/{}.bin", hex::encode(url.as_str()));
        let key = ResourceKey::new(self.asset_root.clone(), rel_path);

        let cancel = CancellationToken::new();
        let res = self
            .assets
            .open_streaming_resource(&key, cancel.clone())
            .await?;

        // Spawn a best-effort background writer for this segment.
        // If multiple callers race, extra writers may occur; the resource contract handles `Sealed`
        // and failure propagation.
        let net = self.net.clone();
        let url = url.clone();
        tokio::spawn({
            let res = res.clone();
            async move {
                let mut stream = match net.stream(url, None).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = res.fail(format!("net error: {e}")).await;
                        return;
                    }
                };

                let mut off: u64 = 0;
                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(chunk_bytes) => {
                            if let Err(e) = res.write_at(off, &chunk_bytes).await {
                                let _ = res.fail(format!("storage write_at error: {e}")).await;
                                return;
                            }
                            off = off.saturating_add(chunk_bytes.len() as u64);
                        }
                        Err(e) => {
                            let _ = res.fail(format!("net stream error: {e}")).await;
                            return;
                        }
                    }
                }

                let _ = res.commit(Some(off)).await;
            }
        });

        // Read-through to bytes: wait for full content once committed, then read all.
        //
        // We don't stream out of this function (callers may), so we read the whole segment after
        // it becomes available.
        let start = Instant::now();
        let mut offset: u64 = 0;
        let mut out = Vec::new();
        let read_chunk: u64 = 64 * 1024;

        loop {
            let end = offset.saturating_add(read_chunk);
            match res.wait_range(offset..end).await? {
                kithara_storage::WaitOutcome::Ready => {
                    let len: usize = (end - offset) as usize;
                    let bytes = res.read_at(offset, len).await?;
                    if bytes.is_empty() {
                        break;
                    }
                    offset = offset.saturating_add(bytes.len() as u64);
                    out.extend_from_slice(&bytes);
                }
                kithara_storage::WaitOutcome::Eof => break,
            }
        }

        let duration = start.elapsed();
        Ok(FetchBytes {
            bytes: Bytes::from(out),
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
