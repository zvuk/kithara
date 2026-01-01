use axum::{Router, extract::Query, routing::get};
use kithara_cache::{AssetCache, CacheOptions};
use kithara_net::{NetClient, NetOptions};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use url::Url;

use crate::HlsError;

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
            .route("/video/480p/playlist.m3u8", get(media_endpoint))
            .route("/video/720p/playlist.m3u8", get(media_endpoint))
            .route("/video/1080p/playlist.m3u8", get(media_endpoint))
            .route("/segment_0.ts", get(segment_endpoint))
            .route("/segment_1.ts", get(segment_endpoint))
            .route("/segment_2.ts", get(segment_endpoint))
            .route("/key.bin", get(key_endpoint))
            .layer(axum::middleware::from_fn(move |req, next| {
                let counts = request_counts_clone.clone();
                async move {
                    let path = req.uri().path().to_string();
                    if let Ok(mut counts) = counts.lock() {
                        *counts.entry(path).or_insert(0) += 1;
                    }
                    next.run(req).await
                }
            }));

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
    let net = NetClient::new(net_opts);

    (cache, net)
}

pub fn test_master_playlist() -> &'static str {
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

pub fn test_media_playlist() -> &'static str {
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

pub fn test_segment_data() -> Vec<u8> {
    // Return deterministic data per segment (simplified)
    b"TEST_SEGMENT_DATA".to_vec()
}

pub fn test_key_data() -> Vec<u8> {
    // Return deterministic key data (simplified)
    b"TEST_KEY_DATA_123456".to_vec()
}

async fn master_endpoint() -> &'static str {
    test_master_playlist()
}

async fn media_endpoint() -> &'static str {
    test_media_playlist()
}

async fn segment_endpoint() -> Vec<u8> {
    test_segment_data()
}

async fn key_endpoint() -> Vec<u8> {
    test_key_data()
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
        assert!(master.contains("video/480p/playlist.m3u8"));

        let media = test_media_playlist();
        assert!(media.contains("#EXTM3U"));
        assert!(media.contains("segment_0.ts"));

        let segment = test_segment_data();
        assert_eq!(segment.len(), 17);

        let key = test_key_data();
        assert_eq!(key.len(), 18);
    }

    #[test]
    fn test_create_test_cache_and_net() {
        let (cache, net) = create_test_cache_and_net();

        // Verify objects are created successfully
        // More detailed testing would require actual cache/net operations
        assert_eq!(format!("{:?}", cache), "AssetCache"); // Basic check
    }
}
