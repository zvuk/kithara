#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{
    AssetStore, Assets, CachedAssets, DiskAssetStore, EvictAssets, LeaseGuard, LeaseResource,
    ProcessedResource, ProcessingAssets, ResourceKey,
};
use kithara_net::{ByteStream, Headers, HttpClient, Net};
use kithara_storage::{Resource as _, ResourceStatus, StreamingResource, StreamingResourceExt};
use tracing::{debug, trace, warn};
use url::Url;

use crate::HlsResult;

pub type StreamingAssetResource = LeaseResource<
    ProcessedResource<StreamingResource, ()>,
    LeaseGuard<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>, ()>>>,
>;

#[derive(Clone)]
pub struct FetchManager<N> {
    assets: AssetStore,
    net: N,
}

impl<N: Net> FetchManager<N> {
    pub fn with_net(assets: AssetStore, net: N) -> Self {
        Self { assets, net }
    }

    pub fn asset_root(&self) -> &str {
        self.assets.asset_root()
    }

    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }

    pub async fn fetch_playlist(&self, url: &Url, rel_path: &str) -> HlsResult<Bytes> {
        self.fetch_atomic_internal(url, rel_path, None, "playlist")
            .await
    }

    pub async fn fetch_key(
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

    /// Fetch init segment completely (no bytes in memory, data goes to disk).
    pub(crate) async fn fetch_init(&self, url: &Url) -> HlsResult<FetchResult> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_streaming_resource(&key).await?;

        let status = res.status().await;
        let from_cache = matches!(status, ResourceStatus::Committed { .. });

        let start = Instant::now();

        if !from_cache {
            Self::download_to_resource(&self.net, url, &res).await;
        }

        let duration = start.elapsed();
        let bytes = match res.status().await {
            ResourceStatus::Committed { final_len } => final_len.unwrap_or(0),
            _ => 0,
        };

        Ok(FetchResult {
            bytes,
            duration,
            from_cache,
        })
    }

    /// Start fetching a segment. Returns cached size if already cached.
    /// Caller iterates over chunks via `ActiveFetch::next_chunk()`.
    pub(crate) async fn start_fetch(&self, url: &Url) -> HlsResult<ActiveFetchResult> {
        let key = ResourceKey::from_url(url);
        let res = self.assets.open_streaming_resource(&key).await?;

        let status = res.status().await;
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit");
            return Ok(ActiveFetchResult::Cached { bytes: len });
        }

        trace!(url = %url, "start_fetch: starting network fetch");
        let net_stream = self.net.stream(url.clone(), None).await?;

        Ok(ActiveFetchResult::Active(ActiveFetch {
            net_stream,
            resource: res,
            offset: 0,
        }))
    }

    /// Download URL to streaming resource (no spawn, runs in current task).
    async fn download_to_resource<TNet: Net>(net: &TNet, url: &Url, res: &StreamingAssetResource) {
        let start_time = Instant::now();
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
}

/// Result of a fetch operation (data stays on disk).
#[derive(Clone, Debug)]
pub struct FetchResult {
    pub bytes: u64,
    pub duration: Duration,
    pub from_cache: bool,
}

/// Result of starting a fetch.
pub enum ActiveFetchResult {
    /// Already cached, no fetch needed.
    Cached { bytes: u64 },
    /// Active fetch in progress.
    Active(ActiveFetch),
}

/// Active fetch context for streaming chunks.
/// Caller iterates via `next_chunk()` and must call `commit()` when done.
pub struct ActiveFetch {
    net_stream: ByteStream,
    resource: StreamingAssetResource,
    offset: u64,
}

impl ActiveFetch {
    /// Get next chunk (writes to disk, returns bytes written).
    /// Returns None when fetch is complete.
    pub async fn next_chunk(&mut self) -> HlsResult<Option<u64>> {
        match self.net_stream.next().await {
            Some(Ok(chunk_bytes)) => {
                let chunk_len = chunk_bytes.len() as u64;
                self.resource.write_at(self.offset, &chunk_bytes).await?;
                self.offset += chunk_len;
                Ok(Some(chunk_len))
            }
            Some(Err(e)) => {
                let _ = self.resource.fail(e.to_string()).await;
                Err(e.into())
            }
            None => Ok(None),
        }
    }

    /// Commit download and return total bytes.
    pub async fn commit(self) -> u64 {
        let _ = self.resource.commit(Some(self.offset)).await;
        self.offset
    }
}

// Backward compatibility: Specialized impl for concrete types
impl FetchManager<HttpClient> {
    pub fn new(assets: AssetStore, net: HttpClient) -> Self {
        Self::with_net(assets, net)
    }
}

/// Type alias for backward compatibility.
/// Use `FetchManager<N>` with custom Net implementation for testing with mocks.
/// Use this alias for production code with HttpClient.
pub type DefaultFetchManager = FetchManager<HttpClient>;

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use kithara_assets::AssetStoreBuilder;
    use kithara_net::MockNet;
    use tempfile::TempDir;
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn test_fetch_playlist_with_mock_net() {
        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();
        let test_url_clone = test_url.clone();
        let playlist_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";

        mock_net
            .expect_get_bytes()
            .times(1)
            .withf(move |url, _| url == &test_url_clone)
            .returning(move |_, _| Ok(Bytes::from_static(playlist_content)));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let result: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&test_url, "playlist.m3u8")
            .await;

        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert_eq!(bytes, Bytes::from_static(playlist_content));
    }

    #[tokio::test]
    async fn test_fetch_playlist_uses_cache() {
        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();
        let test_url_clone = test_url.clone();
        let playlist_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";

        mock_net
            .expect_get_bytes()
            .times(1)
            .withf(move |url, _| url == &test_url_clone)
            .returning(move |_, _| Ok(Bytes::from_static(playlist_content)));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let result1: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&test_url, "playlist.m3u8")
            .await;
        assert!(result1.is_ok());

        let result2: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&test_url, "playlist.m3u8")
            .await;
        assert!(result2.is_ok());
        assert_eq!(result1.unwrap(), result2.unwrap());
    }

    #[tokio::test]
    async fn test_fetch_key_with_mock_net() {
        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let test_url = Url::parse("http://example.com/key.bin").unwrap();
        let test_url_clone = test_url.clone();
        let key_content = vec![0u8; 16];

        mock_net
            .expect_get_bytes()
            .times(1)
            .withf(move |url, _| url == &test_url_clone)
            .returning(move |_, _| Ok(Bytes::from(key_content.clone())));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let result: HlsResult<Bytes> = fetch_manager.fetch_key(&test_url, "key.bin", None).await;

        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert_eq!(bytes.len(), 16);
    }

    #[tokio::test]
    async fn test_fetch_with_network_error() {
        use kithara_net::NetError;

        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();
        let test_url_clone = test_url.clone();

        mock_net
            .expect_get_bytes()
            .times(1)
            .withf(move |url, _| url == &test_url_clone)
            .returning(|_, _| Err(NetError::Timeout));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let result: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&test_url, "playlist.m3u8")
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_matcher_url_path_matching() {
        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let master_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";
        let media_content = b"#EXTINF:4.0\nseg0.bin\n";

        mock_net
            .expect_get_bytes()
            .withf(|url, _| url.path().ends_with("/master.m3u8"))
            .times(1)
            .returning(move |_, _| Ok(Bytes::from_static(master_content)));

        mock_net
            .expect_get_bytes()
            .withf(|url, _| url.path().ends_with("/v0.m3u8"))
            .times(1)
            .returning(move |_, _| Ok(Bytes::from_static(media_content)));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let master_url = Url::parse("http://example.com/master.m3u8").unwrap();
        let master: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&master_url, "master.m3u8")
            .await;
        assert!(master.is_ok());

        let media_url = Url::parse("http://example.com/v0.m3u8").unwrap();
        let media: HlsResult<Bytes> = fetch_manager.fetch_playlist(&media_url, "v0.m3u8").await;
        assert!(media.is_ok());
    }

    #[tokio::test]
    async fn test_matcher_url_domain_matching() {
        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let content = b"test content";

        mock_net
            .expect_get_bytes()
            .withf(|url, _| url.host_str() == Some("cdn1.example.com"))
            .returning(move |_, _| Ok(Bytes::from_static(content)));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let url = Url::parse("http://cdn1.example.com/file.m3u8").unwrap();
        let result: HlsResult<Bytes> = fetch_manager.fetch_playlist(&url, "file.m3u8").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_matcher_headers_matching() {
        use std::collections::HashMap;

        use kithara_net::Headers;

        let temp_dir = TempDir::new().unwrap();
        let assets = AssetStoreBuilder::new()
            .asset_root("test")
            .root_dir(temp_dir.path())
            .build();

        let mut mock_net = MockNet::new();
        let key_content = vec![0u8; 16];

        mock_net
            .expect_get_bytes()
            .withf(|_, headers| {
                if let Some(h) = headers {
                    h.get("Authorization").is_some()
                } else {
                    false
                }
            })
            .returning(move |_, _| Ok(Bytes::from(key_content.clone())));

        let fetch_manager = FetchManager::with_net(assets, mock_net);

        let url = Url::parse("http://example.com/key.bin").unwrap();
        let mut headers_map = HashMap::new();
        headers_map.insert("Authorization".to_string(), "Bearer token".to_string());
        let headers = Some(Headers::from(headers_map));

        let result: HlsResult<Bytes> = fetch_manager.fetch_key(&url, "key.bin", headers).await;

        assert!(result.is_ok());
    }
}
