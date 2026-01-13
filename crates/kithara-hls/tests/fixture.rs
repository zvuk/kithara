use std::{collections::HashMap, sync::Arc, time::Duration};

use aes::Aes128;
use axum::{Router, routing::get};
use cbc::{
    Encryptor,
    cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
};
use kithara_assets::{AssetStore, EvictConfig};
pub use kithara_hls::HlsError;
use kithara_net::{HttpClient, NetOptions};
use rstest::fixture;
use tempfile::TempDir;
use tokio::net::TcpListener;
use url::Url;

pub type HlsResult<T> = Result<T, HlsError>;

pub struct AbrTestServer {
    base_url: String,
}

impl AbrTestServer {
    pub async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://127.0.0.1:{}", addr.port());

        let master = master_playlist;

        let app =
            Router::new()
                .route(
                    "/master.m3u8",
                    get(move || {
                        let master = master.clone();
                        async move { master }
                    }),
                )
                .route(
                    "/v0.m3u8",
                    get(move || async move { media_playlist(0, init) }),
                )
                .route(
                    "/v1.m3u8",
                    get(move || async move { media_playlist(1, init) }),
                )
                .route(
                    "/v2.m3u8",
                    get(move || async move { media_playlist(2, init) }),
                )
                .route(
                    "/seg/v0_0.bin",
                    get(move || async move {
                        segment_data(0, 0, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v0_1.bin",
                    get(move || async move {
                        segment_data(0, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v0_2.bin",
                    get(move || async move {
                        segment_data(0, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_0.bin",
                    get(move || async move {
                        segment_data(1, 0, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_1.bin",
                    get(move || async move {
                        segment_data(1, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v1_2.bin",
                    get(move || async move {
                        segment_data(1, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v2_0.bin",
                    get(move || async move { segment_data(2, 0, segment0_delay, 50_000).await }),
                )
                .route(
                    "/seg/v2_1.bin",
                    get(move || async move {
                        segment_data(2, 1, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route(
                    "/seg/v2_2.bin",
                    get(move || async move {
                        segment_data(2, 2, Duration::from_millis(1), 200_000).await
                    }),
                )
                .route("/init/v0.bin", get(|| async { init_data(0) }))
                .route("/init/v1.bin", get(|| async { init_data(1) }))
                .route("/init/v2.bin", get(|| async { init_data(2) }));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self { base_url }
    }

    pub fn url(&self, path: &str) -> HlsResult<Url> {
        format!("{}{}", self.base_url, path)
            .parse()
            .map_err(|e| HlsError::InvalidUrl(format!("Invalid test URL: {}", e)))
    }
}

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
            .route("/master-init.m3u8", get(master_with_init_endpoint))
            .route("/v0.m3u8", get(|| async { test_media_playlist(0) }))
            .route("/v1.m3u8", get(|| async { test_media_playlist(1) }))
            .route("/v2.m3u8", get(|| async { test_media_playlist(2) }))
            .route(
                "/v0-init.m3u8",
                get(|| async { test_media_playlist_with_init(0) }),
            )
            .route(
                "/v1-init.m3u8",
                get(|| async { test_media_playlist_with_init(1) }),
            )
            .route(
                "/v2-init.m3u8",
                get(|| async { test_media_playlist_with_init(2) }),
            )
            .route(
                "/video/480p/playlist.m3u8",
                get(|| async { test_media_playlist(1) }),
            )
            .route("/seg/v0_0.bin", get(|| async { test_segment_data(0, 0) }))
            .route("/seg/v0_1.bin", get(|| async { test_segment_data(0, 1) }))
            .route("/seg/v0_2.bin", get(|| async { test_segment_data(0, 2) }))
            .route("/seg/v1_0.bin", get(|| async { test_segment_data(1, 0) }))
            .route("/seg/v1_1.bin", get(|| async { test_segment_data(1, 1) }))
            .route("/seg/v1_2.bin", get(|| async { test_segment_data(1, 2) }))
            .route("/seg/v2_0.bin", get(|| async { test_segment_data(2, 0) }))
            .route("/seg/v2_1.bin", get(|| async { test_segment_data(2, 1) }))
            .route("/seg/v2_2.bin", get(|| async { test_segment_data(2, 2) }))
            .route("/init/v0.bin", get(|| async { test_init_data(0) }))
            .route("/init/v1.bin", get(|| async { test_init_data(1) }))
            .route("/init/v2.bin", get(|| async { test_init_data(2) }))
            .route("/key.bin", get(key_endpoint))
            .route("/aes/key.bin", get(|| async { aes128_key_bytes() }))
            .route("/aes/seg0.bin", get(|| async { aes128_ciphertext() }))
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

#[derive(Clone)]
pub struct TestAssets {
    assets: AssetStore,
    _temp_dir: Arc<TempDir>,
}

impl TestAssets {
    #[allow(dead_code)]
    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }
}

pub fn create_test_assets() -> TestAssets {
    // NOTE: The assets/cache API has been redesigned.
    // Keep the temp dir alive by storing it inside the returned wrapper.
    let temp_dir = TempDir::new().unwrap();
    let temp_dir = Arc::new(temp_dir);

    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());

    TestAssets {
        assets,
        _temp_dir: temp_dir,
    }
}

pub fn create_test_net() -> HttpClient {
    let net_opts = NetOptions::default();
    HttpClient::new(net_opts)
}

pub fn master_playlist(v0_bw: u64, v1_bw: u64, v2_bw: u64) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH={v0_bw}
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v1_bw}
v1.m3u8
#EXT-X-STREAM-INF:BANDWIDTH={v2_bw}
v2.m3u8
"#
    )
}

pub fn media_playlist(variant: usize, init: bool) -> String {
    let mut s = String::new();
    s.push_str(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
"#,
    );
    if init {
        s.push_str(&format!("#EXT-X-MAP:URI=\"init/v{}.bin\"\n", variant));
    }
    for i in 0..3 {
        s.push_str("#EXTINF:4.0,\n");
        s.push_str(&format!("seg/v{}_{}.bin\n", variant, i));
    }
    s.push_str("#EXT-X-ENDLIST\n");
    s
}

pub async fn segment_data(
    variant: usize,
    segment: usize,
    delay: Duration,
    total_len: usize,
) -> Vec<u8> {
    if delay != Duration::ZERO {
        tokio::time::sleep(delay).await;
    }
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    if data.len() < total_len {
        data.extend(std::iter::repeat(b'A').take(total_len - data.len()));
    }
    data
}

pub fn init_data(variant: usize) -> Vec<u8> {
    format!("V{}-INIT:", variant).into_bytes()
}

pub fn aes128_key_bytes() -> Vec<u8> {
    b"0123456789abcdef".to_vec()
}

pub fn aes128_iv() -> [u8; 16] {
    [0u8; 16]
}

pub fn aes128_plaintext_segment() -> Vec<u8> {
    b"V0-SEG-0:DRM-PLAINTEXT".to_vec()
}

pub fn aes128_ciphertext() -> Vec<u8> {
    let key = aes128_key_bytes();
    let iv = aes128_iv();
    let mut data = aes128_plaintext_segment();
    let plain_len = data.len();
    data.resize(plain_len + 16, 0);
    let encryptor = Encryptor::<Aes128>::new((&key[..16]).into(), (&iv).into());
    let cipher = encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut data, plain_len)
        .expect("aes128 encrypt");
    cipher.to_vec()
}

#[fixture]
pub async fn abr_server_default() -> AbrTestServer {
    AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(1),
    )
    .await
}

#[fixture]
pub fn assets_fixture() -> TestAssets {
    create_test_assets()
}

#[fixture]
pub fn net_fixture() -> HttpClient {
    create_test_net()
}

#[fixture]
pub fn abr_cache_and_net() -> (TestAssets, HttpClient) {
    (create_test_assets(), create_test_net())
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

pub fn test_master_playlist_with_init() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0-init.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.42c01e,mp4a.40.2"
v1-init.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080,CODECS="avc1.42c01e,mp4a.40.2"
v2-init.m3u8
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

pub fn test_media_playlist_with_init(variant: usize) -> String {
    format!(
        r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="init/v{}.bin"
#EXTINF:4.0,
seg/v{}_0.bin
#EXTINF:4.0,
seg/v{}_1.bin
#EXTINF:4.0,
seg/v{}_2.bin
#EXT-X-ENDLIST
"#,
        variant, variant, variant, variant
    )
}

pub fn test_segment_data(variant: usize, segment: usize) -> Vec<u8> {
    // Return deterministic data per segment with prefix as per spec
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");
    data
}

pub fn test_init_data(variant: usize) -> Vec<u8> {
    let prefix = format!("V{}-INIT:", variant);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_INIT_DATA");
    data
}

pub fn test_key_data() -> Vec<u8> {
    // Return deterministic key data (simplified)
    b"TEST_KEY_DATA_123456".to_vec()
}

async fn master_endpoint() -> &'static str {
    test_master_playlist()
}

async fn master_with_init_endpoint() -> &'static str {
    test_master_playlist_with_init()
}

async fn key_endpoint() -> Vec<u8> {
    test_key_data()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::{fixture, rstest};

    use super::*;

    #[fixture]
    async fn test_server_fixture() -> TestServer {
        TestServer::new().await
    }

    #[fixture]
    fn test_assets_fixture() -> TestAssets {
        create_test_assets()
    }

    #[fixture]
    fn test_net_fixture() -> HttpClient {
        create_test_net()
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_server_creation(#[future] test_server_fixture: TestServer) {
        let server = test_server_fixture.await;

        // Verify server URL is valid
        let url = server.url("/master.m3u8").unwrap();
        assert!(url.as_str().contains("127.0.0.1"));
        assert!(url.as_str().ends_with("/master.m3u8"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_request_counting(#[future] test_server_fixture: TestServer) {
        let server = test_server_fixture.await;

        // Make some requests (would need HTTP client in real test)
        let initial_count = server.get_request_count("/master.m3u8");
        assert_eq!(initial_count, 0);

        // Request counting would be verified by actually making HTTP requests
        // For now, just test the counting mechanism works
    }

    #[rstest]
    #[case(0)]
    #[case(1)]
    #[case(2)]
    fn test_test_data_consistency(#[case] variant: usize) {
        // Verify test data is consistent
        let master = test_master_playlist();
        assert!(master.contains("#EXTM3U"));
        assert!(master.contains("v0.m3u8"));

        let media = test_media_playlist(variant);
        assert!(media.contains("#EXTM3U"));
        assert!(media.contains(&format!("seg/v{}_0.bin", variant)));

        let segment = test_segment_data(variant, 0);
        let expected_prefix = format!("V{}-SEG-0:", variant);
        assert!(segment.starts_with(expected_prefix.as_bytes()));
        assert_eq!(segment.len(), 26); // "V{}-SEG-0:" (9) + "TEST_SEGMENT_DATA" (17) = 26 bytes

        let key = test_key_data();
        assert_eq!(key.len(), 20); // "TEST_KEY_DATA_123456" = 20 bytes
    }

    #[rstest]
    #[case(0, 0, 26)]
    #[case(1, 1, 26)]
    #[case(2, 2, 26)]
    #[case(0, 2, 26)]
    fn test_segment_data_variants(
        #[case] variant: usize,
        #[case] segment: usize,
        #[case] expected_len: usize,
    ) {
        let data = test_segment_data(variant, segment);
        let expected_prefix = format!("V{}-SEG-{}:", variant, segment);
        assert!(data.starts_with(expected_prefix.as_bytes()));
        assert_eq!(data.len(), expected_len);
    }

    #[rstest]
    #[case(0)]
    #[case(1)]
    #[case(2)]
    fn test_init_data_variants(#[case] variant: usize) {
        let data = test_init_data(variant);
        let expected_prefix = format!("V{}-INIT:", variant);
        assert!(data.starts_with(expected_prefix.as_bytes()));
        assert_eq!(data.len(), 22); // "V{}-INIT:" (9) + "TEST_INIT_DATA" (13) = 22 bytes
    }

    #[rstest]
    #[case(0)]
    #[case(1)]
    #[case(2)]
    fn test_media_playlist_variants(#[case] variant: usize) {
        let playlist = test_media_playlist(variant);
        assert!(playlist.contains("#EXTM3U"));
        assert!(playlist.contains("#EXT-X-VERSION:6"));
        assert!(playlist.contains(&format!("seg/v{}_0.bin", variant)));
        assert!(playlist.contains(&format!("seg/v{}_1.bin", variant)));
        assert!(playlist.contains(&format!("seg/v{}_2.bin", variant)));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
    }

    #[rstest]
    #[case(0)]
    #[case(1)]
    #[case(2)]
    fn test_media_playlist_with_init_variants(#[case] variant: usize) {
        let playlist = test_media_playlist_with_init(variant);
        assert!(playlist.contains("#EXTM3U"));
        assert!(playlist.contains("#EXT-X-VERSION:6"));
        assert!(playlist.contains(&format!("#EXT-X-MAP:URI=\"init/v{}.bin\"", variant)));
        assert!(playlist.contains(&format!("seg/v{}_0.bin", variant)));
        assert!(playlist.contains("#EXT-X-ENDLIST"));
    }

    #[rstest]
    fn test_fixture_creation() {
        let assets = create_test_assets();
        let _net = create_test_net();

        let _ = assets;
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn test_fixture_creation_async() {
        let assets = create_test_assets();
        let _net = create_test_net();

        let _ = assets;
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn abr_server_fixture_produces_urls(#[future] abr_server_default: AbrTestServer) {
        let server = abr_server_default.await;

        let url = server.url("/master.m3u8").unwrap();
        assert!(url.as_str().contains("127.0.0.1"));
        assert!(url.as_str().ends_with("/master.m3u8"));
    }

    #[rstest]
    fn abr_playlist_helpers_build_variants() {
        let master = master_playlist(100_000, 200_000, 300_000);
        assert!(master.contains("v0.m3u8"));
        assert!(master.contains("BANDWIDTH=300000"));

        let media = media_playlist(1, true);
        assert!(media.contains("#EXT-X-MAP:URI=\"init/v1.bin\""));
        assert!(media.contains("seg/v1_2.bin"));
    }
}
