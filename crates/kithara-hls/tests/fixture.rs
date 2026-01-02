use axum::{Router, routing::get};
use kithara_cache::{AssetCache, CacheOptions};
use kithara_net::{NetClient, NetOptions};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use url::Url;

pub use kithara_hls::HlsError;

pub type HlsResult<T> = Result<T, HlsError>;

pub struct TestServer {
    base_url: String,
    request_counts: Arc<std::sync::Mutex<HashMap<String, usize>>>,
}

impl TestServer {
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let request_counts = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let request_counts_clone = request_counts.clone();

        let app = Router::new()
            .route("/master.m3u8", get(master_endpoint))
            .route("/v0.m3u8", get(|| async { test_media_playlist(0) }))
            .route("/v1.m3u8", get(|| async { test_media_playlist(1) }))
            .route("/v2.m3u8", get(|| async { test_media_playlist(2) }))
            .route("/seg/v0_0.bin", get(|| async { test_segment_data(0, 0) }))
            .route("/seg/v0_1.bin", get(|| async { test_segment_data(0, 1) }))
            .route("/seg/v0_2.bin", get(|| async { test_segment_data(0, 2) }))
            .route("/seg/v1_0.bin", get(|| async { test_segment_data(1, 0) }))
            .route("/seg/v1_1.bin", get(|| async { test_segment_data(1, 1) }))
            .route("/seg/v1_2.bin", get(|| async { test_segment_data(1, 2) }))
            .route("/seg/v2_0.bin", get(|| async { test_segment_data(2, 0) }))
            .route("/seg/v2_1.bin", get(|| async { test_segment_data(2, 1) }))
            .route("/seg/v2_2.bin", get(|| async { test_segment_data(2, 2) }))
            .route("/key.bin", get(key_endpoint))
            .layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let counts = request_counts_clone.clone();
                    async move {
                        let path = req.uri().path().to_string();
                        if let Ok(mut counts) = counts.lock() {
                            *counts.entry(path).or_insert(0) += 1;
                        }
                        next.run(req).await
                    }
                },
            ));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            base_url,
            request_counts,
        }
    }

    pub fn url(&self, path: &str) -> HlsResult<Url> {
        format!("{}{}", self.base_url, path)
            .parse()
            .map_err(|e| HlsError::InvalidUrl(format!("Invalid test URL: {}", e)))
    }

    pub fn get_request_count(&self, path: &str) -> usize {
        self.request_counts
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .unwrap_or(0)
    }
}

pub fn create_test_cache_and_net() -> (AssetCache, NetClient) {
    let cache_opts = CacheOptions {
        max_bytes: 1024 * 1024,
        root_dir: None,
    };
    let cache = AssetCache::open(cache_opts).unwrap();

    let net_opts = NetOptions::default();
    let net = NetClient::new(net_opts).expect("Failed to create NetClient");

    (cache, net)
}

pub fn test_master_playlist() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.42c01e,mp4a.40.2"
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080,CODECS="avc1.42c01e,mp4a.40.2"
v2.m3u8
"#
}

pub fn test_media_playlist(variant: usize) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
        variant, variant, variant
    )
}

pub fn test_segment_data(variant: usize, segment: usize) -> Vec<u8> {
    // Return deterministic data per segment with prefix as per spec
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");
    data
}

pub fn test_key_data() -> Vec<u8> {
    // Return deterministic key data (simplified)
    b"TEST_KEY_DATA_123456".to_vec()
}

async fn master_endpoint() -> &'static str {
    test_master_playlist()
}

async fn key_endpoint() -> Vec<u8> {
    test_key_data()
}

// Helper function to create test master playlist (from src/fixture.rs)
pub fn create_test_master_playlist() -> hls_m3u8::MasterPlaylist<'static> {
    let playlist_str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID="vid",NAME="720p",BANDWIDTH=2000000,RESOLUTION=1280x720
#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2"
video/720p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480,CODECS="avc1.64001f,mp4a.40.2"
video/480p/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=640x360,CODECS="avc1.64001f,mp4a.40.2"
video/360p/playlist.m3u8
"#;

    hls_m3u8::MasterPlaylist::try_from(playlist_str).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server_creation() {
        let server = TestServer::new().await;

        // Verify server URL is valid
        let url = server.url("/master.m3u8").unwrap();
        assert!(url.as_str().contains("127.0.0.1"));
        assert!(url.as_str().ends_with("/master.m3u8"));
    }

    #[tokio::test]
    async fn test_request_counting() {
        let server = TestServer::new().await;

        // Make some requests (would need HTTP client in real test)
        let initial_count = server.get_request_count("/master.m3u8");
        assert_eq!(initial_count, 0);

        // Request counting would be verified by actually making HTTP requests
        // For now, just test the counting mechanism works
    }

    #[test]
    fn test_test_data_consistency() {
        // Verify test data is consistent
        let master = test_master_playlist();
        assert!(master.contains("#EXTM3U"));
        assert!(master.contains("v0.m3u8"));

        let media = test_media_playlist(0);
        assert!(media.contains("#EXTM3U"));
        assert!(media.contains("seg/v0_0.bin"));

        let segment = test_segment_data(0, 0);
        assert_eq!(segment.len(), 26); // "V0-SEG-0:" (9) + "TEST_SEGMENT_DATA" (17) = 26 bytes

        let key = test_key_data();
        assert_eq!(key.len(), 20); // "TEST_KEY_DATA_123456" = 20 bytes
    }

    #[test]
    fn test_fixture_creation() {
        let (cache, _net) = create_test_cache_and_net();

        // Verify objects are created successfully
        // More detailed testing would require actual cache/net operations
        assert!(format!("{:?}", cache).contains("AssetCache")); // Basic check
    }
}
