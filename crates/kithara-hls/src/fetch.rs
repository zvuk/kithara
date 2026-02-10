#![forbid(unsafe_code)]

//! Fetch layer: network fetch + disk cache + playlist management + segment loading.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetResource, AssetsBackend, ResourceKey};
use kithara_net::{Headers, HttpClient, Net};
use kithara_storage::{ResourceExt, ResourceStatus};
use kithara_stream::{ContainerFormat, Writer, WriterItem};
use parking_lot::RwLock;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{
    HlsError, HlsResult,
    parsing::{
        MasterPlaylist, MediaPlaylist, SegmentKey, VariantId, VariantStream, parse_master_playlist,
        parse_media_playlist,
    },
};

// Types

/// Segment type: initialization segment or media segment with index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Initialization segment (fMP4 only, contains codec metadata).
    Init,
    /// Media segment with index in the playlist.
    Media(usize),
}

impl SegmentType {
    /// Get media segment index, or None for init segment.
    pub fn media_index(self) -> Option<usize> {
        match self {
            Self::Media(idx) => Some(idx),
            Self::Init => None,
        }
    }

    /// Check if this is an init segment.
    pub fn is_init(self) -> bool {
        matches!(self, Self::Init)
    }
}

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_type: SegmentType,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
    pub len: u64,
    pub container: Option<ContainerFormat>,
}

/// Result of starting a fetch.
pub enum FetchResult {
    /// Already cached, no fetch needed.
    Cached { bytes: u64 },
    /// Active fetch in progress via Writer stream.
    /// Includes the resource so the caller can commit after download completes.
    Active(Writer, AssetResource),
}

// Loader trait

/// Generic segment loader.
#[expect(async_fn_in_trait)]
#[cfg_attr(test, unimock::unimock(api = LoaderMock))]
pub trait Loader: Send + Sync {
    /// Load segment and return metadata with real size (after processing).
    async fn load_segment(&self, variant: usize, segment_index: usize) -> HlsResult<SegmentMeta>;

    /// Get number of variants from master playlist.
    fn num_variants(&self) -> usize;

    /// Get total segments in variant's media playlist.
    async fn num_segments(&self, variant: usize) -> HlsResult<usize>;
}

// FetchManager

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

/// Unified HLS fetch manager.
///
/// Handles low-level network fetch + disk cache, playlist parsing/caching,
/// and implements `Loader` for segment loading.
#[derive(Clone)]
pub struct FetchManager<N> {
    backend: AssetsBackend,
    net: N,
    cancel: CancellationToken,
    // Playlist state
    master_url: Option<Url>,
    base_url: Option<Url>,
    master: Arc<OnceCell<MasterPlaylist>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
    num_variants_cache: Arc<RwLock<Option<usize>>>,
}

impl<N: Net> FetchManager<N> {
    pub fn with_net(backend: AssetsBackend, net: N, cancel: CancellationToken) -> Self {
        Self {
            backend,
            net,
            cancel,
            master_url: None,
            base_url: None,
            master: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
            num_variants_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Set master playlist URL (required for Loader functionality).
    pub fn with_master_url(mut self, url: Url) -> Self {
        self.master_url = Some(url);
        self
    }

    /// Set base URL override for resolving relative URLs.
    pub fn with_base_url(mut self, url: Option<Url>) -> Self {
        self.base_url = url;
        self
    }

    pub fn asset_root(&self) -> &str {
        self.backend.asset_root()
    }

    pub fn backend(&self) -> &AssetsBackend {
        &self.backend
    }

    // ---- Low-level fetch ----

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
        let res = self.backend.open_resource(&key)?;

        let mut buf = kithara_assets::byte_pool().get();
        let n = res.read_into(&mut buf)?;
        if n > 0 {
            debug!(
                url = %url,
                asset_root = %self.asset_root(),
                rel_path = %rel_path,
                bytes = n,
                resource_kind,
                "kithara-hls: cache hit"
            );
            return Ok(Bytes::copy_from_slice(&buf));
        }

        debug!(
            url = %url,
            asset_root = %self.asset_root(),
            rel_path = %rel_path,
            resource_kind,
            "kithara-hls: cache miss -> fetching from network"
        );

        let bytes = self.net.get_bytes(url.clone(), headers).await?;
        res.write_all(&bytes)?;

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

    /// Start fetching a segment. Returns cached size if already cached.
    pub(crate) async fn start_fetch(&self, url: &Url) -> HlsResult<FetchResult> {
        let key = ResourceKey::from_url(url);
        let res = self.backend.open_resource(&key)?;

        let status = res.status();
        if let ResourceStatus::Committed { final_len } = status {
            let len = final_len.unwrap_or(0);
            trace!(url = %url, len, "start_fetch: cache hit");
            return Ok(FetchResult::Cached { bytes: len });
        }

        trace!(url = %url, "start_fetch: starting network fetch");
        let net_stream = self.net.stream(url.clone(), None).await?;
        let writer = Writer::new(net_stream, res.clone(), self.cancel.clone());

        Ok(FetchResult::Active(writer, res))
    }

    // ---- Playlist management ----

    pub async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let master = self
            .master
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "master_playlist", parse_master_playlist)
                    .await
            })
            .await?;

        Ok(master.clone())
    }

    pub async fn media_playlist(
        &self,
        url: &Url,
        variant_id: VariantId,
    ) -> HlsResult<MediaPlaylist> {
        let cell = {
            let mut guard = self.media.write();
            guard
                .entry(variant_id)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let playlist = cell
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "media_playlist", |bytes| {
                    parse_media_playlist(bytes, variant_id)
                })
                .await
            })
            .await?;

        Ok(playlist.clone())
    }

    pub fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let resolved = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {e}"))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {e}")))?
        };

        Ok(resolved)
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let bytes = self.fetch_playlist(url, basename).await?;
        parse(&bytes)
    }

    // ---- Loader helpers ----

    fn master_url(&self) -> HlsResult<&Url> {
        self.master_url
            .as_ref()
            .ok_or_else(|| HlsError::InvalidUrl("master_url not set on FetchManager".to_string()))
    }

    async fn load_media_playlist(&self, variant: usize) -> HlsResult<(Url, MediaPlaylist)> {
        let master_url = self.master_url()?;
        let master = self.master_playlist(master_url).await?;

        let variant_stream = master
            .variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("variant {}", variant)))?;

        let media_url = self.resolve_url(master_url, &variant_stream.uri)?;
        let playlist = self.media_playlist(&media_url, VariantId(variant)).await?;

        Ok((media_url, playlist))
    }

    /// Get Content-Length for a URL using HEAD request.
    ///
    /// Returns the size in bytes if Content-Length header is present and parseable.
    pub async fn get_content_length(&self, url: &Url) -> HlsResult<u64> {
        let headers = self.net.head(url.clone(), None).await?;
        let content_length = headers
            .get("content-length")
            .or_else(|| headers.get("Content-Length"))
            .ok_or_else(|| {
                HlsError::InvalidUrl(format!(
                    "No Content-Length header in HEAD response for {}",
                    url
                ))
            })?;

        content_length.parse::<u64>().map_err(|e| {
            HlsError::InvalidUrl(format!(
                "Invalid Content-Length '{}' for {}: {}",
                content_length, url, e
            ))
        })
    }

    /// Get URLs for all segments in a variant (init + media segments).
    ///
    /// Returns (`init_url`, `media_urls`) where `init_url` is None for TS streams.
    /// Does NOT download segments — only resolves URLs from playlist.
    pub async fn get_segment_urls(&self, variant: usize) -> HlsResult<(Option<Url>, Vec<Url>)> {
        let (media_url, playlist) = self.load_media_playlist(variant).await?;

        let init_url = if let Some(ref init_segment) = playlist.init_segment {
            Some(media_url.join(&init_segment.uri).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve init segment URL: {}", e))
            })?)
        } else {
            None
        };

        let media_urls: Result<Vec<Url>, HlsError> = playlist
            .segments
            .iter()
            .map(|segment| {
                media_url.join(&segment.uri).map_err(|e| {
                    HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e))
                })
            })
            .collect();

        Ok((init_url, media_urls?))
    }
}

impl FetchManager<HttpClient> {
    pub fn new(backend: AssetsBackend, net: HttpClient, cancel: CancellationToken) -> Self {
        Self::with_net(backend, net, cancel)
    }
}

pub type DefaultFetchManager = FetchManager<HttpClient>;

// Loader impl for FetchManager

impl<N: Net> Loader for FetchManager<N> {
    async fn load_segment(&self, variant: usize, segment_index: usize) -> HlsResult<SegmentMeta> {
        let (media_url, playlist) = self.load_media_playlist(variant).await?;

        let container = if playlist.init_segment.is_some() {
            Some(ContainerFormat::Fmp4)
        } else {
            Some(ContainerFormat::MpegTs)
        };

        let segment_type = if segment_index == usize::MAX {
            SegmentType::Init
        } else {
            SegmentType::Media(segment_index)
        };

        if segment_type.is_init() {
            trace!(variant, "looking for init segment in playlist");
            let init_segment = playlist.init_segment.as_ref().ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "init segment not found in variant {} playlist",
                    variant
                ))
            })?;

            let init_url = media_url.join(&init_segment.uri).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve init segment URL: {}", e))
            })?;

            let fetch_result = self.start_fetch(&init_url).await?;

            let init_len = match fetch_result {
                FetchResult::Cached { bytes } => {
                    trace!(variant, bytes, "init segment already cached");
                    bytes
                }
                FetchResult::Active(mut writer, res) => {
                    debug!(variant, url = %init_url, "downloading init segment");
                    let mut total = 0u64;
                    while let Some(result) = writer.next().await {
                        match result {
                            Ok(WriterItem::ChunkWritten { len, .. }) => {
                                total += len as u64;
                            }
                            Ok(WriterItem::StreamEnded { total_bytes }) => {
                                total = total_bytes;
                                break;
                            }
                            Err(e) => return Err(e.into()),
                        }
                    }
                    if total == 0 {
                        res.fail("init segment: 0 bytes downloaded".to_string());
                        return Err(HlsError::SegmentNotFound(
                            "init segment download yielded 0 bytes".to_string(),
                        ));
                    }
                    // Commit resource explicitly after successful download
                    res.commit(Some(total)).map_err(HlsError::from)?;
                    debug!(variant, total, "init segment downloaded");
                    total
                }
            };

            return Ok(SegmentMeta {
                variant,
                segment_type: SegmentType::Init,
                sequence: 0,
                url: init_url,
                duration: None,
                key: None,
                len: init_len,
                container,
            });
        }

        let segment = playlist.segments.get(segment_index).ok_or_else(|| {
            HlsError::SegmentNotFound(format!(
                "segment {} not found in variant {} playlist",
                segment_index, variant
            ))
        })?;

        let segment_url = media_url
            .join(&segment.uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        let fetch_result = self.start_fetch(&segment_url).await?;

        let segment_len = match fetch_result {
            FetchResult::Cached { bytes } => bytes,
            FetchResult::Active(mut writer, res) => {
                let mut total = 0u64;
                while let Some(result) = writer.next().await {
                    match result {
                        Ok(WriterItem::ChunkWritten { len, .. }) => {
                            total += len as u64;
                        }
                        Ok(WriterItem::StreamEnded { total_bytes }) => {
                            total = total_bytes;
                            break;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                if total == 0 {
                    res.fail("segment: 0 bytes downloaded".to_string());
                    return Err(HlsError::SegmentNotFound(format!(
                        "segment {} download yielded 0 bytes",
                        segment_index
                    )));
                }
                // Commit resource explicitly after successful download
                res.commit(Some(total)).map_err(HlsError::from)?;
                total
            }
        };

        Ok(SegmentMeta {
            variant,
            segment_type,
            sequence: segment.sequence,
            url: segment_url,
            duration: Some(segment.duration),
            key: segment.key.clone(),
            len: segment_len,
            container,
        })
    }

    fn num_variants(&self) -> usize {
        if let Some(cached) = *self.num_variants_cache.read() {
            return cached;
        }

        if let Some(variants) = self.master_variants() {
            let count = variants.len();
            *self.num_variants_cache.write() = Some(count);
            return count;
        }

        0
    }

    async fn num_segments(&self, variant: usize) -> HlsResult<usize> {
        let (_media_url, playlist) = self.load_media_playlist(variant).await?;
        Ok(playlist.segments.len())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use kithara_assets::AssetStoreBuilder;
    use kithara_net::NetMock;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use unimock::{MockFn, Unimock, matching};
    use url::Url;

    use super::*;

    // ---- FetchManager tests ----

    #[tokio::test]
    async fn test_fetch_playlist_with_mock_net() {
        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let playlist_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";
        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Ok(Bytes::from_static(playlist_content))),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

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
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();
        let playlist_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";

        // get_bytes called exactly once — second fetch should use cache
        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Ok(Bytes::from_static(playlist_content))),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

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
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let test_url = Url::parse("http://example.com/key.bin").unwrap();
        let key_content = vec![0u8; 16];

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Ok(Bytes::from(key_content))),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

        let result: HlsResult<Bytes> = fetch_manager.fetch_key(&test_url, "key.bin", None).await;

        assert!(result.is_ok());
        let bytes = result.unwrap();
        assert_eq!(bytes.len(), 16);
    }

    #[tokio::test]
    async fn test_fetch_with_network_error() {
        use kithara_net::NetError;

        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let test_url = Url::parse("http://example.com/playlist.m3u8").unwrap();

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

        let result: HlsResult<Bytes> = fetch_manager
            .fetch_playlist(&test_url, "playlist.m3u8")
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_matcher_url_path_matching() {
        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let master_content = b"#EXTM3U\n#EXT-X-VERSION:6\n";
        let media_content = b"#EXTINF:4.0\nseg0.bin\n";

        let mock_net = Unimock::new(NetMock::get_bytes.stub(|each| {
            each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                .returns(Ok(Bytes::from_static(master_content)));
            each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                .returns(Ok(Bytes::from_static(media_content)));
        }));

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

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
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let content = b"test content";

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!((url, _) if url.host_str() == Some("cdn1.example.com")))
                .returns(Ok(Bytes::from_static(content))),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

        let url = Url::parse("http://cdn1.example.com/file.m3u8").unwrap();
        let result: HlsResult<Bytes> = fetch_manager.fetch_playlist(&url, "file.m3u8").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_matcher_headers_matching() {
        use std::collections::HashMap;

        use kithara_net::Headers;

        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("test"))
            .root_dir(temp_dir.path())
            .build();

        let key_content = vec![0u8; 16];

        let mock_net = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!((_, headers) if headers.as_ref().is_some_and(|h| h.get("Authorization").is_some())))
                .returns(Ok(Bytes::from(key_content))),
        );

        let fetch_manager = FetchManager::with_net(backend, mock_net, CancellationToken::new());

        let url = Url::parse("http://example.com/key.bin").unwrap();
        let mut headers_map = HashMap::new();
        headers_map.insert("Authorization".to_string(), "Bearer token".to_string());
        let headers = Some(Headers::from(headers_map));

        let result: HlsResult<Bytes> = fetch_manager.fetch_key(&url, "key.bin", headers).await;

        assert!(result.is_ok());
    }

    // ---- Loader tests ----

    fn create_test_meta(variant: usize, segment_index: usize, len: u64) -> SegmentMeta {
        SegmentMeta {
            variant,
            segment_type: SegmentType::Media(segment_index),
            sequence: segment_index as u64,
            url: Url::parse(&format!(
                "http://test.com/v{}/seg{}.ts",
                variant, segment_index
            ))
            .expect("valid URL"),
            duration: Some(Duration::from_secs(4)),
            key: None,
            len,
            container: Some(ContainerFormat::MpegTs),
        }
    }

    #[tokio::test]
    async fn test_mock_loader_basic() {
        let loader = Unimock::new((
            LoaderMock::num_variants
                .some_call(matching!())
                .returns(3_usize),
            LoaderMock::load_segment
                .some_call(matching!(0, 5))
                .answers(&|_, variant, idx| Ok(create_test_meta(variant, idx, 200_000))),
            LoaderMock::num_segments
                .some_call(matching!(0))
                .returns(Ok(100_usize)),
        ));

        assert_eq!(loader.num_variants(), 3);
        assert_eq!(loader.num_segments(0).await.unwrap(), 100);

        let meta = loader.load_segment(0, 5).await.unwrap();
        assert_eq!(meta.variant, 0);
        assert_eq!(meta.segment_type.media_index(), Some(5));
        assert_eq!(meta.len, 200_000);
    }

    #[tokio::test]
    async fn test_mock_loader_multi_variant() {
        let loader = Unimock::new(LoaderMock::load_segment.each_call(matching!(_, _)).answers(
            &|_, variant, idx| {
                Ok(create_test_meta(
                    variant,
                    idx,
                    200_000 + variant as u64 * 50_000,
                ))
            },
        ));

        let meta0 = loader.load_segment(0, 1).await.unwrap();
        let meta1 = loader.load_segment(1, 1).await.unwrap();
        let meta2 = loader.load_segment(2, 1).await.unwrap();

        assert_eq!(meta0.len, 200_000);
        assert_eq!(meta1.len, 250_000);
        assert_eq!(meta2.len, 300_000);
    }

    // ---- Partial segment tests ----

    #[tokio::test]
    async fn test_load_segment_stream_error_returns_err() {
        use kithara_net::{ByteStream, NetError};

        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("partial-test"))
            .root_dir(temp_dir.path())
            .build();

        let mock_net = Unimock::new((
            NetMock::get_bytes.stub(|each| {
                each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                    .returns(Ok(Bytes::from(
                        "#EXTM3U\n\
                         #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
                         v0.m3u8\n",
                    )));
                each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                    .returns(Ok(Bytes::from(
                        "#EXTM3U\n\
                         #EXT-X-TARGETDURATION:4\n\
                         #EXT-X-MEDIA-SEQUENCE:0\n\
                         #EXT-X-PLAYLIST-TYPE:VOD\n\
                         #EXTINF:4.0,\n\
                         seg0.ts\n\
                         #EXT-X-ENDLIST\n",
                    )));
            }),
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(&|_, _url, _headers| {
                    let stream = futures::stream::iter(vec![
                        Ok(Bytes::from(vec![0xFF; 1000])),
                        Err(NetError::Timeout),
                    ]);
                    Ok(Box::pin(stream) as ByteStream)
                }),
        ));

        let master_url = Url::parse("http://test.com/master.m3u8").unwrap();
        let fetch = FetchManager::with_net(backend, mock_net, CancellationToken::new())
            .with_master_url(master_url);

        let result = fetch.load_segment(0, 0).await;
        assert!(
            result.is_err(),
            "stream error should propagate as load_segment error"
        );
    }

    #[tokio::test]
    async fn test_load_segment_empty_stream_returns_err() {
        use kithara_net::{ByteStream, NetError};

        let temp_dir = TempDir::new().unwrap();
        let backend = AssetStoreBuilder::new()
            .asset_root(Some("partial-test"))
            .root_dir(temp_dir.path())
            .build();

        let mock_net = Unimock::new((
            NetMock::get_bytes.stub(|each| {
                each.call(matching!((url, _) if url.path().ends_with("/master.m3u8")))
                    .returns(Ok(Bytes::from(
                        "#EXTM3U\n\
                         #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
                         v0.m3u8\n",
                    )));
                each.call(matching!((url, _) if url.path().ends_with("/v0.m3u8")))
                    .returns(Ok(Bytes::from(
                        "#EXTM3U\n\
                         #EXT-X-TARGETDURATION:4\n\
                         #EXT-X-MEDIA-SEQUENCE:0\n\
                         #EXT-X-PLAYLIST-TYPE:VOD\n\
                         #EXTINF:4.0,\n\
                         seg0.ts\n\
                         #EXT-X-ENDLIST\n",
                    )));
            }),
            NetMock::stream
                .some_call(matching!((url, _) if url.path().contains("seg0")))
                .answers(&|_, _url, _headers| {
                    let stream = futures::stream::empty::<Result<Bytes, NetError>>();
                    Ok(Box::pin(stream) as ByteStream)
                }),
        ));

        let master_url = Url::parse("http://test.com/master.m3u8").unwrap();
        let fetch = FetchManager::with_net(backend, mock_net, CancellationToken::new())
            .with_master_url(master_url);

        let result = fetch.load_segment(0, 0).await;
        assert!(
            result.is_err(),
            "empty segment (0 bytes) should be treated as error"
        );
    }
}
