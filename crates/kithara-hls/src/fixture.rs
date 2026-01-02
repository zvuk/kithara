//! Test fixtures for kithara-hls tests

use kithara_cache::AssetCache;
use kithara_net::NetClient;
use url::Url;

pub struct TestServer {
    base_url: Url,
}

impl TestServer {
    pub fn new() -> Self {
        Self {
            base_url: Url::parse("http://localhost:8080").unwrap(),
        }
    }

    pub fn url(&self, path: &str) -> crate::HlsResult<Url> {
        Ok(self
            .base_url
            .join(path)
            .map_err(|e| crate::HlsError::InvalidUrl(e.to_string()))?)
    }
}

pub fn create_test_cache_and_net() -> (AssetCache, NetClient) {
    // Placeholder implementations - in real tests these would be properly mocked
    todo!("Implement test fixtures")
}

// Helper function to create test master playlist
pub fn create_test_master_playlist() -> hls_m3u8::MasterPlaylist {
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
