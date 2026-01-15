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
    assets: AssetStore,
    net: HttpClient,
    read_chunk_bytes: u64,
}

impl FetchManager {
    pub fn new(assets: AssetStore, net: HttpClient) -> Self {
        Self::new_with_read_chunk(assets, net, 64 * 1024)
    }

    pub fn new_with_read_chunk(assets: AssetStore, net: HttpClient, read_chunk_bytes: u64) -> Self {
        Self {
            assets,
            net,
            read_chunk_bytes,
        }
    }

    pub fn asset_root(&self) -> &str {
        self.assets.asset_root()
    }

    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }

    pub async fn fetch_playlist_atomic(&self, url: &Url, rel_path: &str) -> HlsResult<Bytes> {
        self.fetch_atomic_internal(url, rel_path, None, "playlist")
            .await
    }

    pub async fn fetch_key_atomic(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        self.fetch_atomic_internal(url, rel_path, headers, "key")
            .await
    }

    async fn fetch_atomic_internal(
        &self,
        url: &Url,
        rel_path: &str,
        headers: Option<Headers>,
        resource_kind: &str,
    ) -> HlsResult<Bytes> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_atomic_resource(&key).await?;

        let cached = res.read().await?;
        if !cached.is_empty() {
            debug!(
                url = %url,
                asset_root = %self.asset_root(),
                rel_path = %rel_path,
                bytes = cached.len(),
                resource_kind,
                "kithara-hls: cache hit"
            );
            return Ok(cached);
        }

        debug!(
            url = %url,
            asset_root = %self.asset_root(),
            rel_path = %rel_path,
            resource_kind,
            "kithara-hls: cache miss -> fetching from network"
        );

        let bytes = self.net.get_bytes(url.clone(), headers).await?;
        res.write(&bytes).await?;

        debug!(
            url = %url,
            asset_root = %self.asset_root(),
            rel_path = %rel_path,
            bytes = bytes.len(),
            resource_kind,
            "kithara-hls: fetched from network and cached"
        );

        Ok(bytes)
    }

    pub async fn probe_content_length(&self, url: &Url) -> HlsResult<Option<u64>> {
        // First check if segment is already in cache
        let key = ResourceKey::from_url(url);
        if let Ok(res) = self.assets.open_streaming_resource(&key).await {
            let status = res.inner().status().await;
            if let ResourceStatus::Committed {
                final_len: Some(len),
            } = status
            {
                trace!(url = %url, len, "probe_content_length: cache hit");
                return Ok(Some(len));
            }
        }

        // Fall back to HEAD request
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

        let key = ResourceKey::from_url(&segment_url);
        let rel_path = key.rel_path();
        self.fetch_streaming_to_bytes_internal(&segment_url, rel_path, &key)
            .await
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        Ok(self.fetch_init_segment_resource(init_url).await?.bytes)
    }

    pub async fn fetch_init_segment_resource(&self, url: &Url) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url(url);
        let rel_path = key.rel_path();

        self.fetch_streaming_to_bytes_internal(url, rel_path, &key)
            .await
    }

    pub async fn fetch_media_segment_resource(&self, url: &Url) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url(url);
        let rel_path = key.rel_path();

        self.fetch_streaming_to_bytes_internal(url, rel_path, &key)
            .await
    }

    pub fn stream_segment_sequence<'a>(
        &'a self,
        media_playlist: &'a MediaPlaylist,
        base_url: &'a Url,
        key_context: Option<&'a KeyContext>,
    ) -> SegmentStream<'a> {
        let me = self.clone();
        let base_url = base_url.clone();

        Box::pin(async_stream::stream! {
            if key_context.is_some() {
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

                let key = ResourceKey::from_url(&segment_url);
                let rel_path = key.rel_path();
                yield me.fetch_streaming_to_bytes_internal(&segment_url, rel_path, &key).await;
            }
        })
    }

    /// Download URL to streaming resource (no spawn, runs in current task).
    async fn download_to_resource(net: &HttpClient, url: &Url, res: &StreamingAssetResource) {
        let start_time = std::time::Instant::now();
        trace!(url = %url, "kithara-hls segment download: START");

        let mut stream = match net.stream(url.clone(), None).await {
            Ok(s) => s,
            Err(e) => {
                warn!(url = %url, error = %e, "kithara-hls download: net open error");
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
                        warn!(url = %url, off, error = %e, "kithara-hls download: write error");
                        let _ = res.fail(format!("storage write_at error: {e}")).await;
                        return;
                    }
                    off = off.saturating_add(chunk_len);
                }
                Err(e) => {
                    warn!(url = %url, off, error = %e, "kithara-hls download: stream error");
                    let _ = res.fail(format!("net stream error: {e}")).await;
                    return;
                }
            }
        }

        let _ = res.commit(Some(off)).await;
        trace!(
            url = %url,
            bytes = off,
            elapsed_ms = start_time.elapsed().as_millis(),
            "kithara-hls segment download: END"
        );
    }

    pub(crate) async fn fetch_streaming_to_bytes_internal(
        &self,
        url: &Url,
        rel_path: &str,
        key: &ResourceKey,
    ) -> HlsResult<FetchBytes> {
        let res = self.assets.open_streaming_resource(key).await?;

        let status = res.inner().status().await;
        let from_cache = matches!(status, ResourceStatus::Committed { .. });

        // Download if not cached (no spawn, runs in current task).
        if !from_cache {
            Self::download_to_resource(&self.net, url, &res).await;
        }

        let read_chunk = self.read_chunk_bytes;
        let mut out = match status {
            ResourceStatus::Committed {
                final_len: Some(final_len),
            } if final_len <= usize::MAX as u64 => Vec::with_capacity(final_len as usize),
            _ => Vec::with_capacity(read_chunk as usize),
        };

        let asset_root = self.asset_root();

        // Read-through to bytes: wait for full content once committed, then read all.
        let start = Instant::now();
        let mut offset: u64 = 0;

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

        let source = if from_cache {
            ThroughputSampleSource::Cache
        } else {
            ThroughputSampleSource::Network
        };

        Ok(FetchBytes {
            bytes: Bytes::from(out),
            source,
            duration,
        })
    }

    /// Download streaming resource and return metadata (no bytes in memory).
    pub(crate) async fn download_streaming(&self, url: &Url) -> HlsResult<FetchMeta> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_streaming_resource(&key).await?;

        let status = res.inner().status().await;
        let from_cache = matches!(status, ResourceStatus::Committed { .. });

        let start = Instant::now();

        if !from_cache {
            // Download directly in current task (no spawn).
            Self::download_to_resource(&self.net, url, &res).await;
        }

        let duration = start.elapsed();

        // Get final length from committed resource.
        let final_len = match res.inner().status().await {
            ResourceStatus::Committed { final_len } => final_len.unwrap_or(0),
            _ => 0,
        };

        let source = if from_cache {
            ThroughputSampleSource::Cache
        } else {
            ThroughputSampleSource::Network
        };

        Ok(FetchMeta {
            len: final_len,
            source,
            duration,
        })
    }
}

/// Metadata without bytes (data stays on disk).
#[derive(Clone, Debug)]
pub struct FetchMeta {
    pub len: u64,
    pub source: ThroughputSampleSource,
    pub duration: Duration,
}
