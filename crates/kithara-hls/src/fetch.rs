#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_assets::{
    AssetResource, AssetStore, DiskAssetStore, EvictAssets, LeaseGuard, ResourceKey,
};
use kithara_net::{Headers, HttpClient, Net as _};
use kithara_storage::{
    Resource as _, ResourceStatus, StreamingResource, StreamingResourceExt, WaitOutcome,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};
use url::Url;

use crate::{
    HlsError, HlsResult, KeyContext, abr::ThroughputSampleSource, playlist::MediaPlaylist,
};

pub type SegmentStream<'a> = Pin<Box<dyn Stream<Item = HlsResult<FetchBytes>> + Send + 'a>>;

pub type StreamingAssetResource =
    AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>;

#[derive(Clone, Debug)]
pub struct FetchBytes {
    pub bytes: Bytes,
    pub source: ThroughputSampleSource,
    pub duration: Duration,
}

#[derive(Clone)]
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

    pub async fn fetch_playlist_atomic(&self, url: &Url, rel_path: &str) -> HlsResult<Bytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);
        let cancel = CancellationToken::new();
        let res = self.assets.open_atomic_resource(&key, cancel).await?;

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

    pub async fn fetch_key_atomic(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);

        let cancel = CancellationToken::new();
        let res = self.assets.open_atomic_resource(&key, cancel).await?;

        let cached = res.read().await?;
        if !cached.is_empty() {
            debug!(
                url = %url,
                asset_root = %self.asset_root,
                rel_path = %rel_path,
                bytes = cached.len(),
                "kithara-hls: key cache hit"
            );
            return Ok(cached);
        }

        debug!(
            url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            "kithara-hls: key cache miss -> fetching from network"
        );

        let bytes = self.net.get_bytes(url.clone(), headers).await?;
        res.write(&bytes).await?;

        debug!(
            url = %url,
            asset_root = %self.asset_root,
            rel_path = %rel_path,
            bytes = bytes.len(),
            "kithara-hls: key fetched from network and cached"
        );

        Ok(bytes)
    }

    pub async fn probe_content_length(&self, url: &Url) -> HlsResult<Option<u64>> {
        let headers: Headers = self.net.head(url.clone(), None).await?;

        fn find_content_length(headers: &Headers) -> Option<u64> {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<u64>().ok())
        }

        Ok(find_content_length(&headers))
    }

    pub async fn fetch_segment(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<Bytes> {
        Ok(self
            .fetch_segment_internal(media_playlist, segment_index, base_url, key_context)
            .await?
            .bytes)
    }

    pub async fn fetch_segment_with_meta(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<FetchBytes> {
        self.fetch_segment_internal(media_playlist, segment_index, base_url, key_context)
            .await
    }

    async fn fetch_segment_internal(
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

        if key_context.is_some() {
            return Err(HlsError::Unimplemented);
        }

        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &segment_url);
        Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            &segment_url,
            key.rel_path(),
        )
        .await
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        Ok(self.fetch_init_segment_resource(init_url).await?.bytes)
    }

    pub async fn fetch_init_segment_resource(&self, url: &Url) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);

        Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            url,
            key.rel_path(),
        )
        .await
    }

    pub async fn fetch_media_segment_resource(&self, url: &Url) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);

        Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            url,
            key.rel_path(),
        )
        .await
    }

    pub async fn open_init_streaming_resource(
        &self,
        url: &Url,
    ) -> HlsResult<StreamingAssetResource> {
        self.open_streaming_resource_with_writer(url).await
    }

    pub async fn open_media_streaming_resource(
        &self,
        url: &Url,
    ) -> HlsResult<StreamingAssetResource> {
        self.open_streaming_resource_with_writer(url).await
    }

    async fn open_streaming_resource_with_writer(
        &self,
        url: &Url,
    ) -> HlsResult<StreamingAssetResource> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), url);
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

        FetchManager::spawn_stream_writer(net, url, res_for_writer);

        Ok(res)
    }

    pub fn stream_segment_sequence<'a>(
        &self,
        media_playlist: &'a MediaPlaylist,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> SegmentStream<'a> {
        let asset_root = self.asset_root.clone();
        let assets = self.assets.clone();
        let net = self.net.clone();
        let base_url = base_url.clone();
        let key_ctx = key_context.cloned();

        Box::pin(async_stream::stream! {
            if key_ctx.is_some() {
                yield Err(HlsError::Unimplemented);
                return;
            }

            for segment in media_playlist.segments.iter() {
                let segment_url = match base_url.join(&segment.uri) {
                    Ok(url) => url,
                    Err(e) => {
                        yield Err(HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)));
                        continue;
                    }
                };

                let key = ResourceKey::from_url_with_asset_root(asset_root.clone(), &segment_url);
                let fetch_result = FetchManager::fetch_streaming_to_bytes_internal(
                    &asset_root, &assets, &net, &segment_url, key.rel_path()
                ).await;

                match fetch_result {
                    Ok(fetch) => {
                        yield Ok(fetch);
                    }
                    Err(e) => yield Err(e),
                }
            }
        })
    }

    pub(crate) fn spawn_stream_writer(net: HttpClient, url: Url, res: StreamingAssetResource) {
        tokio::spawn(async move {
            let mut stream = match net.stream(url.clone(), None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(url = %url, error = %e, "kithara-hls streaming writer: net open error");
                    let _ = res.fail(format!("net error: {e}")).await;
                    return;
                }
            };

            let mut off: u64 = 0;
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk_bytes) => {
                        let chunk_len = chunk_bytes.len() as u64;
                        if let Err(e) = res.write_at(off, &chunk_bytes).await {
                            warn!(
                                url = %url,
                                off,
                                error = %e,
                                "kithara-hls streaming writer: storage write_at error"
                            );
                            let _ = res.fail(format!("storage write_at error: {e}")).await;
                            return;
                        }
                        off = off.saturating_add(chunk_len);
                    }
                    Err(e) => {
                        warn!(
                            url = %url,
                            off,
                            error = %e,
                            "kithara-hls streaming writer: net stream error"
                        );
                        let _ = res.fail(format!("net stream error: {e}")).await;
                        return;
                    }
                }
            }

            let _ = res.commit(Some(off)).await;
        });
    }

    // Helper for stream_segment_sequence to avoid creating new FetchManager
    pub(crate) async fn fetch_streaming_to_bytes_internal(
        asset_root: &str,
        assets: &AssetStore,
        net: &HttpClient,
        url: &Url,
        rel_path: &str,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(asset_root.to_string(), url);

        // Intentionally quiet: this path can be called a lot during playback.
        // Keep only warnings/errors and a single "done" summary.

        let cancel = CancellationToken::new();
        let res = assets.open_streaming_resource(&key, cancel.clone()).await?;

        // Spawn a best-effort background writer for this segment.
        // If multiple callers race, extra writers may occur; the resource contract handles `Sealed`
        // and failure propagation.
        let net = net.clone();
        let url = url.clone();

        let status = res.inner().status().await;
        if !matches!(status, ResourceStatus::Committed { .. }) {
            FetchManager::spawn_stream_writer(net, url, res.clone());
        }

        // Read-through to bytes: wait for full content once committed, then read all.
        let start = Instant::now();
        let mut offset: u64 = 0;
        let mut out = Vec::new();
        let read_chunk: u64 = 64 * 1024;

        loop {
            let end = offset.saturating_add(read_chunk);

            let outcome = res.wait_range(offset..end).await?;

            match outcome {
                WaitOutcome::Ready => {
                    let len: usize = (end - offset) as usize;
                    let bytes = res.read_at(offset, len).await?;

                    if bytes.is_empty() {
                        // Storage returned empty after reporting Ready: this is unexpected and can
                        // lead to "end of stream" at the decoder level.
                        warn!(
                            asset_root = %asset_root,
                            rel_path = %rel_path,
                            offset,
                            "kithara-hls segment reader: empty-after-ready"
                        );
                        break;
                    }
                    offset = offset.saturating_add(bytes.len() as u64);
                    out.extend_from_slice(&bytes);
                }
                WaitOutcome::Eof => {
                    break;
                }
            }
        }

        let duration = start.elapsed();
        trace!(
            asset_root = %asset_root,
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
}
