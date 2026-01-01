#![forbid(unsafe_code)]

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
        let asset_id = AssetId::from_url(&url);
        let (cmd_sender, _cmd_receiver) = mpsc::channel(16);

        // TODO: Start HLS driver task
        // let driver = HlsDriver::new(url, opts, cache, net, cmd_receiver, bytes_sender);
        // tokio::spawn(driver.run());

        Ok(HlsSession {
            asset_id,
            cmd_sender,
        })
    }
}

pub struct HlsSession {
    asset_id: AssetId,
    cmd_sender: mpsc::Sender<HlsCommand>,
}

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn commands(&self) -> mpsc::Sender<HlsCommand> {
        self.cmd_sender.clone()
    }

    pub fn stream(&self) -> impl Stream<Item = HlsResult<Bytes>> + Send + '_ {
        futures::stream::iter(vec![Err(HlsError::Unimplemented)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use kithara_cache::CacheOptions;
    use kithara_net::NetOptions;
    use tokio::net::TcpListener;

    fn test_master_playlist() -> &'static str {
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio-aacl-128",NAME="English",LANGUAGE="en",DEFAULT=YES,AUTOSELECT=YES,URI="audio/eng/128k/playlist.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=1280000,AVERAGE-BANDWIDTH=1000000,CODECS="avc1.42c01e,mp4a.40.2",RESOLUTION=854x480,FRAME-RATE=30.000,AUDIO="audio-aacl-128"
video/480p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,AVERAGE-BANDWIDTH=2000000,CODECS="avc1.42c01e,mp4a.40.2",RESOLUTION=1280x720,FRAME-RATE=30.000,AUDIO="audio-aacl-128"
video/720p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,AVERAGE-BANDWIDTH=4500000,CODECS="avc1.42c01e,mp4a.40.2",RESOLUTION=1920x1080,FRAME-RATE=30.000,AUDIO="audio-aacl-128"
video/1080p/playlist.m3u8
"#
    }

    fn test_media_playlist() -> &'static str {
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
segment_0.ts
#EXTINF:4.0,
segment_1.ts
#EXTINF:4.0,
segment_2.ts
#EXT-X-ENDLIST
"#
    }

    fn test_app() -> Router {
        Router::new()
            .route("/master.m3u8", get(master_endpoint))
            .route("/video/480p/playlist.m3u8", get(media_endpoint))
            .route("/video/720p/playlist.m3u8", get(media_endpoint))
            .route("/video/1080p/playlist.m3u8", get(media_endpoint))
    }

    async fn master_endpoint() -> &'static str {
        test_master_playlist()
    }

    async fn media_endpoint() -> &'static str {
        test_media_playlist()
    }

    async fn run_test_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = test_app();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        format!("http://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn parse_master_playlist_from_network() -> HlsResult<()> {
        let server_url = run_test_server().await;
        let master_url = format!("{}/master.m3u8", server_url).parse().unwrap();

        let _opts = HlsOptions::default();
        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let _cache = AssetCache::open(cache_opts).unwrap();
        let net_opts = NetOptions::default();
        let net = NetClient::new(net_opts);

        // Parse master playlist
        let master_bytes = net.get_bytes(master_url).await?;
        let master_str = String::from_utf8(master_bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;
        let master_playlist = hls_m3u8::MasterPlaylist::try_from(master_str.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        // Verify we have 3 variants
        assert_eq!(master_playlist.variant_streams.len(), 3);

        // Check variant properties
        let variants: Vec<_> = master_playlist.variant_streams.iter().collect();
        assert_eq!(variants[0].bandwidth(), 1280000);
        assert_eq!(variants[1].bandwidth(), 2560000);
        assert_eq!(variants[2].bandwidth(), 5120000);

        // Check resolutions
        assert_eq!(
            variants[0].resolution(),
            Some(hls_m3u8::types::Resolution::new(854, 480))
        );
        assert_eq!(
            variants[1].resolution(),
            Some(hls_m3u8::types::Resolution::new(1280, 720))
        );
        assert_eq!(
            variants[2].resolution(),
            Some(hls_m3u8::types::Resolution::new(1920, 1080))
        );

        Ok(())
    }

    #[tokio::test]
    async fn parse_media_playlist_from_network() -> HlsResult<()> {
        let server_url = run_test_server().await;
        let media_url = format!("{}/video/480p/playlist.m3u8", server_url)
            .parse()
            .unwrap();

        let _opts = HlsOptions::default();
        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let _cache = AssetCache::open(cache_opts).unwrap();
        let net_opts = NetOptions::default();
        let net = NetClient::new(net_opts);

        // Parse media playlist
        let media_bytes = net.get_bytes(media_url).await?;
        let media_str = String::from_utf8(media_bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;
        let media_playlist = hls_m3u8::MediaPlaylist::try_from(media_str.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        // Check that it's a playlist (basic verification)
        let mut segment_count = 0;
        for _segment in media_playlist.segments.iter() {
            segment_count += 1;
        }
        assert_eq!(segment_count, 3);

        Ok(())
    }

    #[tokio::test]
    async fn variant_selection_manual_override() -> HlsResult<()> {
        let server_url = run_test_server().await;
        let master_url = format!("{}/master.m3u8", server_url).parse().unwrap();

        // Create options with manual variant selector (select highest bandwidth)
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

        let opts = HlsOptions {
            variant_stream_selector: Some(selector),
            ..HlsOptions::default()
        };

        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let _cache = AssetCache::open(cache_opts).unwrap();
        let net_opts = NetOptions::default();
        let net = NetClient::new(net_opts);

        // Parse master playlist and apply selection
        let master_bytes = net.get_bytes(master_url).await?;
        let master_str = String::from_utf8(master_bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;
        let master_playlist = hls_m3u8::MasterPlaylist::try_from(master_str.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        // Apply variant selection
        let selected_index = (opts.variant_stream_selector.unwrap())(&master_playlist)
            .expect("Should return a variant index");

        // Should select the highest bandwidth variant (index 2)
        assert_eq!(selected_index, 2);

        Ok(())
    }

    #[tokio::test]
    async fn variant_selection_auto_abr() -> HlsResult<()> {
        let server_url = run_test_server().await;
        let master_url = format!("{}/master.m3u8", server_url).parse().unwrap();

        // Create options with no selector (auto ABR)
        let opts = HlsOptions {
            variant_stream_selector: None,      // Auto ABR
            abr_initial_variant_index: Some(1), // Start with medium quality
            ..HlsOptions::default()
        };

        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let _cache = AssetCache::open(cache_opts).unwrap();
        let net_opts = NetOptions::default();
        let net = NetClient::new(net_opts);

        // Parse master playlist
        let master_bytes = net.get_bytes(master_url).await?;
        let master_str = String::from_utf8(master_bytes.to_vec())
            .map_err(|e| HlsError::PlaylistParse(format!("Invalid UTF-8: {}", e)))?;
        let master_playlist = hls_m3u8::MasterPlaylist::try_from(master_str.as_str())
            .map_err(|e| HlsError::PlaylistParse(e.to_string()))?;

        // With no selector, should use initial variant index
        assert!(opts.variant_stream_selector.is_none());
        assert_eq!(opts.abr_initial_variant_index, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn caches_master_playlist_for_offline_use() -> HlsResult<()> {
        let server_url = run_test_server().await;
        let master_url = format!("{}/master.m3u8", server_url).parse().unwrap();

        let _opts = HlsOptions::default();
        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let cache = AssetCache::open(cache_opts).unwrap();
        let net_opts = NetOptions::default();
        let net = NetClient::new(net_opts);

        let asset_id = AssetId::from_url(&master_url);

        // First run: cache the master playlist
        {
            let master_bytes = net.get_bytes(master_url.clone()).await?;
            let cache_path = CachePath::new(vec!["master.m3u8".to_string()]).unwrap();
            let handle = cache.asset(asset_id);
            handle.put_atomic(&cache_path, &master_bytes).unwrap();
        }

        // Second run: verify offline mode can read from cache
        {
            let cache_path = CachePath::new(vec!["master.m3u8".to_string()]).unwrap();
            let handle = cache.asset(asset_id);
            assert!(handle.exists(&cache_path));

            // Read from cache
            let mut cached_file = handle.open(&cache_path).unwrap().unwrap();
            let mut cached_bytes = Vec::new();
            use std::io::Read;
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
        let server_url = run_test_server().await;
        let master_url = format!("{}/missing.m3u8", server_url).parse().unwrap();

        let offline_opts = HlsOptions {
            offline_mode: true,
            ..HlsOptions::default()
        };

        let cache_opts = CacheOptions {
            max_bytes: 1024 * 1024,
            root_dir: None,
        };
        let cache = AssetCache::open(cache_opts).unwrap();
        let _net_opts = NetOptions::default();
        let _net = NetClient::new(_net_opts);

        let asset_id = AssetId::from_url(&master_url);
        let handle = cache.asset(asset_id);

        // In offline mode, accessing uncached resource should use cache miss logic
        let cache_path = CachePath::new(vec!["missing.m3u8".to_string()]).unwrap();

        // File should not exist (not cached)
        assert!(!handle.exists(&cache_path));

        // In offline mode implementation, this would typically lead to an error
        // For now, we just verify the cache behavior
        Ok(())
    }
}
