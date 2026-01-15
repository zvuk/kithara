#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{
    AssetResource, AssetStore, Assets, CachedAssets, DiskAssetStore, EvictAssets, LeaseGuard,
    ResourceKey,
};
use kithara_net::{Headers, HttpClient, Net as _};
use kithara_storage::{Resource as _, ResourceStatus, StreamingResource, StreamingResourceExt};
use tracing::{debug, trace, warn};
use url::Url;

use crate::{
    HlsResult,
    abr::{ThroughputSample, ThroughputSampleSource},
};

pub type StreamingAssetResource =
    AssetResource<StreamingResource, LeaseGuard<CachedAssets<EvictAssets<DiskAssetStore>>>>;

#[derive(Clone)]
pub struct FetchManager {
    assets: AssetStore,
    net: HttpClient,
}

impl FetchManager {
    pub fn new(assets: AssetStore, net: HttpClient) -> Self {
        Self { assets, net }
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

        let headers: Headers = self.net.head(url.clone(), None).await?;

        fn find_content_length(headers: &Headers) -> Option<u64> {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<u64>().ok())
        }

        Ok(find_content_length(&headers))
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

    /// Download streaming resource and return metadata (no bytes in memory).
    pub(crate) async fn download_streaming(&self, url: &Url) -> HlsResult<FetchMeta> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_streaming_resource(&key).await?;

        let status = res.inner().status().await;
        let from_cache = matches!(status, ResourceStatus::Committed { .. });

        let start = Instant::now();

        if !from_cache {
            Self::download_to_resource(&self.net, url, &res).await;
        }

        let duration = start.elapsed();

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

    /// Prepare streaming download - returns expected length and download context.
    ///
    /// Caller is responsible for driving the download via `drive_download`.
    pub(crate) async fn prepare_streaming(&self, url: &Url) -> HlsResult<StreamingPrepareResult> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_streaming_resource(&key).await?;

        let status = res.inner().status().await;

        // Check if already cached
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "prepare_streaming: cache hit");
            return Ok(StreamingPrepareResult {
                expected_len: Some(len),
                download: None, // No download needed
            });
        }

        // Get Content-Length via HEAD request
        let expected_len = self.probe_content_length(url).await.unwrap_or(None);

        trace!(url = %url, expected_len, "prepare_streaming: prepared");

        Ok(StreamingPrepareResult {
            expected_len,
            download: Some(DownloadContext {
                url: url.clone(),
                resource: res,
            }),
        })
    }

    /// Drive a prepared download to completion. Returns throughput sample.
    pub(crate) async fn drive_download(&self, ctx: DownloadContext) -> ThroughputSample {
        let start = Instant::now();
        Self::download_to_resource(&self.net, &ctx.url, &ctx.resource).await;
        let duration = start.elapsed();
        let at = Instant::now();

        let final_len = match ctx.resource.inner().status().await {
            ResourceStatus::Committed { final_len } => final_len.unwrap_or(0),
            _ => 0,
        };

        ThroughputSample {
            bytes: final_len,
            duration,
            at,
            source: ThroughputSampleSource::Network,
        }
    }
}

/// Result of preparing a streaming download.
#[derive(Debug)]
pub struct StreamingPrepareResult {
    pub expected_len: Option<u64>,
    pub download: Option<DownloadContext>,
}

/// Context for driving a download.
pub struct DownloadContext {
    pub url: Url,
    pub resource: StreamingAssetResource,
}

impl std::fmt::Debug for DownloadContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadContext")
            .field("url", &self.url)
            .finish()
    }
}

/// Metadata without bytes (data stays on disk).
#[derive(Clone, Debug)]
pub struct FetchMeta {
    pub len: u64,
    pub source: ThroughputSampleSource,
    pub duration: Duration,
}
