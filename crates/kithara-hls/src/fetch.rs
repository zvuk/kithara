#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_assets::{AssetStore, ResourceKey};
use kithara_net::{Headers, HttpClient, Net as _};
use kithara_storage::{Resource as _, ResourceStatus, StreamingResourceExt};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};
use url::Url;

use crate::{
    HlsError, HlsEvent, HlsResult, KeyContext, abr::ThroughputSampleSource, playlist::MediaPlaylist,
};

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

    pub fn net(&self) -> &HttpClient {
        &self.net
    }

    pub async fn probe_content_length(&self, url: &Url) -> HlsResult<Option<u64>> {
        let headers: Headers = self.net.head(url.clone(), None).await?;

        let len = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .and_then(|s: &str| s.parse::<u64>().ok());

        Ok(len)
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

        let segment_uri = &segment.uri;
        let segment_url = base_url
            .join(segment_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            base_url = %base_url,
            segment_uri = %segment_uri,
            segment_url = %segment_url,
            "kithara-hls fetch_segment begin"
        );

        let fetch = self.fetch_resource(&segment_url, "segment.ts").await?;

        // Decrypt if needed
        let out = if let Some(key_ctx) = key_context {
            self.decrypt_segment(fetch.bytes, key_ctx).await?
        } else {
            fetch.bytes
        };

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            bytes = out.len(),
            "kithara-hls fetch_segment done"
        );

        Ok(out)
    }

    pub async fn fetch_segment_with_meta(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<FetchBytes> {
        let segment = media_playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::SegmentNotFound(format!("Index {}", segment_index)))?;

        let segment_uri = &segment.uri;
        let segment_url = base_url
            .join(segment_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            base_url = %base_url,
            segment_uri = %segment_uri,
            segment_url = %segment_url,
            "kithara-hls fetch_segment_with_meta begin"
        );

        let mut fetch = self.fetch_resource(&segment_url, "segment.ts").await?;
        if let Some(key_ctx) = key_context {
            fetch.bytes = self.decrypt_segment(fetch.bytes, key_ctx).await?;
        }

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            bytes = fetch.bytes.len(),
            duration_ms = fetch.duration.as_millis(),
            "kithara-hls fetch_segment_with_meta done"
        );

        Ok(fetch)
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        trace!(
            asset_root = %self.asset_root,
            init_url = %init_url,
            variant_id = 0usize,
            "kithara-hls fetch_init_segment begin"
        );
        let out = self.fetch_init_segment_resource(0, init_url).await?.bytes;
        trace!(
            asset_root = %self.asset_root,
            init_url = %init_url,
            variant_id = 0usize,
            bytes = out.len(),
            "kithara-hls fetch_init_segment done"
        );
        Ok(out)
    }

    pub async fn fetch_init_segment_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);

        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            rel_path = %key.rel_path,
            "kithara-hls fetch_init_segment_resource"
        );

        self.fetch_streaming_to_bytes(url, &key.rel_path).await
    }

    pub async fn fetch_media_segment_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);

        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            rel_path = %key.rel_path,
            "kithara-hls fetch_media_segment_resource"
        );

        self.fetch_streaming_to_bytes(url, &key.rel_path).await
    }

    pub async fn open_init_streaming_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<
        kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    > {
        self.open_init_streaming_resource_with_events(variant_id, url, None)
            .await
    }

    pub async fn open_init_streaming_resource_with_events(
        &self,
        variant_id: usize,
        url: &Url,
        events: Option<broadcast::Sender<HlsEvent>>,
    ) -> HlsResult<
        kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    > {
        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            "kithara-hls open_init_streaming_resource"
        );
        self.open_streaming_resource_with_writer(url, variant_id, events)
            .await
    }

    pub async fn open_media_streaming_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<
        kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    > {
        self.open_media_streaming_resource_with_events(variant_id, url, None)
            .await
    }

    pub async fn open_media_streaming_resource_with_events(
        &self,
        variant_id: usize,
        url: &Url,
        events: Option<broadcast::Sender<HlsEvent>>,
    ) -> HlsResult<
        kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    > {
        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            "kithara-hls open_media_streaming_resource"
        );

        self.open_streaming_resource_with_writer(url, variant_id, events)
            .await
    }

    async fn open_streaming_resource_with_writer(
        &self,
        url: &Url,
        variant_id: usize,
        events: Option<broadcast::Sender<HlsEvent>>,
    ) -> HlsResult<
        kithara_assets::AssetResource<
            kithara_storage::StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    > {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);
        let cancel = CancellationToken::new();
        let res = self
            .assets
            .open_streaming_resource(&key, cancel.clone())
            .await?;

        let status = res.inner().status().await;

        // Spawn best-effort writer (net -> storage). Readers will observe availability via the same handle.
        if matches!(status, ResourceStatus::Committed { .. }) {
            return Ok(res);
        }

        let net = self.net.clone();
        let url = url.clone();
        let res_for_writer = res.clone();
        let events_for_writer = events.clone();
        let content_length_for_progress = self.probe_content_length(&url).await.ok().flatten();
        let started_at = Instant::now();

        tokio::spawn(async move {
            if let Some(ev) = &events_for_writer {
                let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                    ev.send(HlsEvent::SegmentStart {
                        variant: variant_id,
                        segment_index: 0,
                        byte_offset: 0,
                    });
            }

            let mut stream = match net.stream(url.clone(), None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(url = %url, error = %e, "kithara-hls streaming writer: net open error");
                    let _ = res_for_writer.fail(format!("net error: {e}")).await;
                    return;
                }
            };

            let mut off: u64 = 0;
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk_bytes) => {
                        let chunk_len = chunk_bytes.len() as u64;
                        if let Err(e) = res_for_writer.write_at(off, &chunk_bytes).await {
                            warn!(
                                url = %url,
                                off,
                                error = %e,
                                "kithara-hls streaming writer: storage write_at error"
                            );
                            let _ = res_for_writer
                                .fail(format!("storage write_at error: {e}"))
                                .await;
                            return;
                        }
                        off = off.saturating_add(chunk_len);
                        if let Some(ev) = &events_for_writer {
                            let percent = content_length_for_progress
                                .map(|len| ((off as f64 / len as f64) * 100.0).min(100.0) as f32);
                            let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                                ev.send(HlsEvent::DownloadProgress {
                                    offset: off,
                                    percent,
                                });
                        }
                    }
                    Err(e) => {
                        warn!(
                            url = %url,
                            off,
                            error = %e,
                            "kithara-hls streaming writer: net stream error"
                        );
                        let _ = res_for_writer.fail(format!("net stream error: {e}")).await;
                        return;
                    }
                }
            }

            let _ = res_for_writer.commit(Some(off)).await;

            if let Some(ev) = &events_for_writer {
                let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                    ev.send(HlsEvent::SegmentComplete {
                        variant: variant_id,
                        segment_index: 0,
                        bytes_transferred: off,
                        duration: started_at.elapsed(),
                    });
            }
        });

        Ok(res)
    }

    pub fn stream_segment_sequence(
        &self,
        media_playlist: MediaPlaylist,
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
            .map(|segment| segment.uri.clone())
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

                // NOTE: variant_id is not plumbed through this API yet; use `0` for now.
                match fetcher.fetch_media_segment_resource(0, &segment_url).await {
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

    async fn fetch_streaming_to_bytes(&self, url: &Url, rel_path: &str) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);

        // Intentionally quiet: this path can be called a lot during playback.
        // Keep only warnings/errors and a single "done" summary.

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
                let mut stream = match net.stream(url.clone(), None).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(url = %url, error = %e, "kithara-hls segment writer: net error");
                        let _ = res.fail(format!("net error: {e}")).await;
                        return;
                    }
                };

                let mut off: u64 = 0;
                while let Some(chunk_result) = stream.next().await {
                    match chunk_result {
                        Ok(chunk_bytes) => {
                            if let Err(e) = res.write_at(off, &chunk_bytes).await {
                                warn!(
                                    url = %url,
                                    off,
                                    error = %e,
                                    "kithara-hls segment writer: storage write_at error"
                                );
                                let _ = res.fail(format!("storage write_at error: {e}")).await;
                                return;
                            }
                            off = off.saturating_add(chunk_bytes.len() as u64);
                        }
                        Err(e) => {
                            warn!(
                                url = %url,
                                off,
                                error = %e,
                                "kithara-hls segment writer: net stream error"
                            );
                            let _ = res.fail(format!("net stream error: {e}")).await;
                            return;
                        }
                    }
                }

                let _ = res.commit(Some(off)).await;
            }
        });

        // Read-through to bytes: wait for full content once committed, then read all.
        let start = Instant::now();
        let mut offset: u64 = 0;
        let mut out = Vec::new();
        let read_chunk: u64 = 64 * 1024;

        loop {
            let end = offset.saturating_add(read_chunk);

            let outcome = res.wait_range(offset..end).await?;

            match outcome {
                kithara_storage::WaitOutcome::Ready => {
                    let len: usize = (end - offset) as usize;
                    let bytes = res.read_at(offset, len).await?;

                    if bytes.is_empty() {
                        // Storage returned empty after reporting Ready: this is unexpected and can
                        // lead to "end of stream" at the decoder level.
                        warn!(
                            asset_root = %self.asset_root,
                            rel_path = %rel_path,
                            offset,
                            "kithara-hls segment reader: empty-after-ready"
                        );
                        break;
                    }
                    offset = offset.saturating_add(bytes.len() as u64);
                    out.extend_from_slice(&bytes);
                }
                kithara_storage::WaitOutcome::Eof => {
                    break;
                }
            }
        }

        let duration = start.elapsed();
        trace!(
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            bytes = out.len(),
            duration_ms = duration.as_millis(),
            "kithara-hls fetch_streaming_to_bytes done"
        );

        Ok(FetchBytes {
            bytes: Bytes::from(out),
            source: ThroughputSampleSource::Network,
            duration,
        })
    }

    async fn fetch_resource(&self, url: &Url, _default_filename: &str) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);

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
