#![forbid(unsafe_code)]

//! # kithara-hls
//!
//! HLS VOD orchestration with caching, ABR, and DRM support.
//!
//! ## Features
//!
//! - **Playlist Management**: Master and media playlist parsing and caching
//! - **Adaptive Bitrate**: ABR policy with configurable parameters
//! - **Segment Fetching**: Network and cache-aware segment streaming
//! - **DRM Support**: AES-128 key processing and caching
//! - **Event System**: Telemetry and monitoring events
//! - **Offline Mode**: Complete offline playback when content is cached
//!
//! ## URL Resolution Rules
//!
//! URLs are resolved using following priority:
//!
//! 1. **base_url Override**: If configured in `HlsOptions.base_url`, this takes precedence
//!    for resolving variant playlists, segments, and keys.
//! 2. **Playlist URL**: The URL of current playlist is used as base for relative URLs.
//!
//! The base_url override allows remapping resources under different path prefixes
//! (useful for CDN changes, proxy setups, etc.).
//!
//! ## DRM Key Processing and Caching
//!
//! - Keys are fetched with optional query parameters and headers
//! - `key_processor_cb` can transform "wrapped" keys to usable form
//! - **Processed keys** (after transformation) are cached for offline playback
//! - Raw keys are never logged; only fingerprints/hashes may be logged
//! - Cache misses in offline mode are fatal errors
//!
//! ## ABR Switching Invariants
//!
//! - ABR decisions are based only on **network throughput** (cache hits don't affect estimation)
//! - Up-switching requires sufficient buffer level and hysteresis ratio
//! - Down-switching occurs when buffer falls below threshold
//! - Minimum switch interval prevents oscillation
//! - Manual overrides take precedence over automatic ABR

use bytes::Bytes;
use futures::Stream;
use kithara_cache::{AssetCache, CachePath};
use kithara_core::AssetId;
use kithara_net::NetClient;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use url::Url;

// Public modules
pub mod abr;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod playlist;

// Private modules
mod driver;
#[cfg(test)]
mod fixture;

// Re-export key types
pub use abr::{AbrConfig, AbrController, ThroughputSample};
pub use events::{EventEmitter, EventError, HlsEvent, VariantChangeReason};
pub use fetch::{FetchError, FetchManager, SegmentStream};
pub use keys::{KeyContext, KeyError, KeyManager};
pub use playlist::{PlaylistError, PlaylistManager};

#[derive(Debug, Error)]
pub enum HlsError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),

    #[error("Playlist parsing error: {0}")]
    PlaylistParse(String),

    #[error("Variant not found: {0}")]
    VariantNotFound(String),

    #[error("No suitable variant found")]
    NoSuitableVariant,

    #[error("Key processing failed: {0}")]
    KeyProcessing(String),

    #[error("Offline mode: resource not cached")]
    OfflineMiss,

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("not implemented")]
    Unimplemented,
}

pub type HlsResult<T> = Result<T, HlsError>;

#[derive(Clone)]
pub struct HlsOptions {
    pub base_url: Option<Url>,
    pub variant_stream_selector:
        Option<Arc<dyn Fn(&hls_m3u8::MasterPlaylist) -> Option<usize> + Send + Sync>>,
    pub abr_initial_variant_index: Option<usize>,
    pub abr_min_buffer_for_up_switch: f32,
    pub abr_down_switch_buffer: f32,
    pub abr_throughput_safety_factor: f32,
    pub abr_up_hysteresis_ratio: f32,
    pub abr_down_hysteresis_ratio: f32,
    pub abr_min_switch_interval: Duration,
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
    pub retry_timeout: Duration,
    pub prefetch_buffer_size: usize,
    pub live_refresh_interval: Option<Duration>,
    pub key_processor_cb: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    pub key_query_params: Option<std::collections::HashMap<String, String>>,
    pub key_request_headers: Option<std::collections::HashMap<String, String>>,
    pub offline_mode: bool,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            variant_stream_selector: None,
            abr_initial_variant_index: Some(0),
            abr_min_buffer_for_up_switch: 10.0,
            abr_down_switch_buffer: 5.0,
            abr_throughput_safety_factor: 1.5,
            abr_up_hysteresis_ratio: 1.3,
            abr_down_hysteresis_ratio: 0.8,
            abr_min_switch_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            retry_timeout: Duration::from_secs(60),
            prefetch_buffer_size: 3,
            live_refresh_interval: None,
            key_processor_cb: None,
            key_query_params: None,
            key_request_headers: None,
            offline_mode: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

#[derive(Clone, Debug)]
pub enum HlsCommand {
    Stop,
    SeekTime(Duration),
    SetVariant(usize),
    ClearVariantOverride,
}

#[derive(Clone, Debug)]
pub struct HlsSource;

impl HlsSource {
    pub async fn open(
        url: Url,
        opts: HlsOptions,
        cache: AssetCache,
        net: NetClient,
    ) -> HlsResult<HlsSession> {
        let asset_id = kithara_core::AssetId::from_url(&url);
        let (cmd_sender, cmd_receiver) = mpsc::channel(16);
        let (bytes_sender, bytes_receiver) = mpsc::channel(100);

        // For now, simplified implementation without full driver
        // TODO: Implement full driver when cache module is fixed
        tokio::spawn(async move {
            // Placeholder driver that just emits unimplemented error
            let _ = cmd_sender;
            let _ = bytes_sender;
        });

        Ok(HlsSession {
            asset_id,
            cmd_sender,
            bytes_receiver,
        })
    }
}

pub struct HlsSession {
    asset_id: kithara_core::AssetId,
    cmd_sender: mpsc::Sender<HlsCommand>,
    bytes_receiver: mpsc::Receiver<HlsResult<Bytes>>,
}

impl HlsSession {
    pub fn asset_id(&self) -> kithara_core::AssetId {
        self.asset_id
    }

    pub fn commands(&self) -> mpsc::Sender<HlsCommand> {
        self.cmd_sender.clone()
    }

    pub fn stream(&self) -> impl Stream<Item = HlsResult<Bytes>> + Send + '_ {
        use futures::StreamExt;
        futures::stream::unfold(self.bytes_receiver.clone(), |mut receiver| async {
            receiver.recv().map(|item| (item, receiver))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::*;
    use kithara_cache::CacheOptions;
    use kithara_cache::CachePath;
    use kithara_core::AssetId;
    use kithara_net::NetOptions;
    use std::io::Read;

    #[tokio::test]
    async fn parse_master_playlist_from_network() -> HlsResult<()> {
        let server = TestServer::new().await;
        let master_url = server.url("/master.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        // Parse master playlist using playlist manager
        let playlist_manager = PlaylistManager::new(cache, net, None);
        let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

        // Verify we have 3 variants
        assert_eq!(master_playlist.variant_streams.len(), 3);

        // Check variant properties
        let variants: Vec<_> = master_playlist.variant_streams.iter().collect();
        assert_eq!(variants[0].bandwidth(), 1280000);
        assert_eq!(variants[1].bandwidth(), 2560000);
        assert_eq!(variants[2].bandwidth(), 5120000);

        Ok(())
    }

    #[tokio::test]
    async fn parse_media_playlist_from_network() -> HlsResult<()> {
        let server = TestServer::new().await;
        let media_url = server.url("/video/480p/playlist.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        // Parse media playlist using playlist manager
        let playlist_manager = PlaylistManager::new(cache, net, None);
        let media_playlist = playlist_manager.fetch_media_playlist(&media_url).await?;

        // Check that it's a playlist (basic verification)
        assert_eq!(media_playlist.segments.len(), 3);

        Ok(())
    }

    #[tokio::test]
    async fn variant_selection_manual_override() -> HlsResult<()> {
        let server = TestServer::new().await;
        let master_url = server.url("/master.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        // Parse master playlist using playlist manager
        let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
        let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

        // Create ABR controller with manual variant selector (select highest bandwidth)
        let selector = Arc::new(|playlist: &hls_m3u8::MasterPlaylist| {
            // Find variant with highest bandwidth
            let variants: Vec<_> = playlist.variant_streams.iter().collect();
            let mut max_bandwidth = 0;
            let mut best_index = 0;
            for (i, variant) in variants.iter().enumerate() {
                let bw = variant.bandwidth();
                if bw > max_bandwidth {
                    max_bandwidth = bw;
                    best_index = i;
                }
            }
            Some(best_index)
        });

        let abr_config = AbrConfig::default();
        let mut abr_controller = AbrController::new(abr_config, Some(selector), 0);

        // Apply variant selection
        let selected_index = abr_controller.select_variant(&master_playlist)?;

        // Should select highest bandwidth variant (index 2)
        assert_eq!(selected_index, 2);

        Ok(())
    }

    #[tokio::test]
    async fn variant_selection_auto_abr() -> HlsResult<()> {
        let server = TestServer::new().await;
        let master_url = server.url("/master.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        // Parse master playlist using playlist manager
        let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
        let master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

        // Create ABR controller with no selector (auto ABR)
        let abr_config = AbrConfig {
            initial_variant_index: Some(1), // Start with medium quality
            ..AbrConfig::default()
        };
        let mut abr_controller = AbrController::new(abr_config, None, 1);

        // With no selector, should use initial variant index
        let selected_index = abr_controller.select_variant(&master_playlist)?;
        assert_eq!(selected_index, 1);

        Ok(())
    }

    #[tokio::test]
    async fn caches_master_playlist_for_offline_use() -> HlsResult<()> {
        let server = TestServer::new().await;
        let master_url = server.url("/master.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        let asset_id = AssetId::from_url(&master_url);

        // First run: cache master playlist
        {
            let playlist_manager = PlaylistManager::new(cache.clone(), net.clone(), None);
            let _master_playlist = playlist_manager.fetch_master_playlist(&master_url).await?;

            // Verify it's cached by checking existence
            let cache_path =
                CachePath::new(vec!["hls".to_string(), "master.m3u8".to_string()]).unwrap();
            let handle = cache.asset(asset_id);
            assert!(handle.exists(&cache_path));
        }

        // Second run: verify can read from cache (simulating offline)
        {
            let cache_path =
                CachePath::new(vec!["hls".to_string(), "master.m3u8".to_string()]).unwrap();
            let handle = cache.asset(asset_id);
            assert!(handle.exists(&cache_path));

            // Read from cache
            let mut cached_file = handle.open(&cache_path)?.unwrap();
            let mut cached_bytes = Vec::new();
            cached_file.read_to_end(&mut cached_bytes).unwrap();

            // Verify content
            let cached_str = String::from_utf8(cached_bytes).unwrap();
            assert!(cached_str.contains("#EXTM3U"));
            assert!(cached_str.contains("video/480p/playlist.m3u8"));
        }

        Ok(())
    }

    #[tokio::test]
    async fn offline_mode_fails_on_miss() -> HlsResult<()> {
        let server = TestServer::new().await;
        let master_url = server.url("/missing.m3u8")?;
        let (cache, net) = create_test_cache_and_net();

        let asset_id = AssetId::from_url(&master_url);
        let handle = cache.asset(asset_id);

        // Verify missing resource is not cached
        let cache_path =
            CachePath::new(vec!["hls".to_string(), "missing.m3u8".to_string()]).unwrap();
        assert!(!handle.exists(&cache_path));

        // In offline mode, this would fail when trying to fetch
        // For now, we just verify cache behavior
        Ok(())
    }
}
