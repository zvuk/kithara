//! HTTP test server for HLS content
//!
//! Provides `TestServer` with routes for master/media playlists and segments.
//! On native: in-process axum server. On WASM: delegates to external fixture server.

// ── Native implementation ──────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex as StdMutex},
    };

    use axum::{Router, routing::get};
    use kithara_test_utils::TestHttpServer;
    use url::Url;

    use super::{
        super::{HlsResult, crypto::*},
        *,
    };

    /// Test HTTP server for HLS content
    pub(crate) struct TestServer {
        pub(super) http: TestHttpServer,
        #[expect(
            dead_code,
            reason = "held for lifetime, used indirectly via middleware closure"
        )]
        request_counts: Arc<StdMutex<HashMap<String, usize>>>,
    }

    impl TestServer {
        pub(crate) async fn new() -> Self {
            let request_counts = Arc::new(StdMutex::new(HashMap::new()));
            let request_counts_clone = request_counts.clone();

            let app = Router::new()
                .route("/master.m3u8", get(master_endpoint))
                .route("/master-init.m3u8", get(master_with_init_endpoint))
                .route("/master-encrypted.m3u8", get(master_encrypted_endpoint))
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
                    "/v0-encrypted.m3u8",
                    get(|| async { test_media_playlist_encrypted(0) }),
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

            let http = TestHttpServer::new(app).await;

            Self {
                http,
                request_counts,
            }
        }

        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub(crate) fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(self.http.url(path))
        }
    }

    // Endpoint helpers
    async fn master_endpoint() -> &'static str {
        test_master_playlist()
    }

    async fn master_with_init_endpoint() -> &'static str {
        test_master_playlist_with_init()
    }

    async fn master_encrypted_endpoint() -> &'static str {
        test_master_playlist_encrypted()
    }

    async fn key_endpoint() -> Vec<u8> {
        test_key_data()
    }
}

// ── WASM implementation ────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use kithara_test_utils::fixture_client;
    use url::Url;

    use super::super::HlsResult;

    /// Remote test server backed by fixture server session.
    pub(crate) struct TestServer {
        session_id: String,
        base_url: Url,
    }

    impl TestServer {
        pub(crate) async fn new() -> Self {
            let resp = fixture_client::create_fixed_hls_session().await;
            Self {
                session_id: resp.session_id,
                base_url: resp.base_url.parse().unwrap(),
            }
        }

        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub(crate) fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(self.base_url.join(path).unwrap())
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            // Best-effort cleanup — fire and forget.
            // On WASM we can't block, so we spawn the delete.
            let id = self.session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                fixture_client::delete_session(&id).await;
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::TestServer;
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::TestServer;

#[kithara::fixture]
pub(crate) async fn test_server() -> TestServer {
    TestServer::new().await
}

// ── Shared content generators (used by native server + tests) ──────

/// Master playlist with standard bitrates
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_master_playlist() -> &'static str {
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

/// Master playlist with init segments
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_master_playlist_with_init() -> &'static str {
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

/// Media playlist for variant
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_media_playlist(variant: usize) -> String {
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

/// Media playlist with init segment
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_media_playlist_with_init(variant: usize) -> String {
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

/// Test segment data with padding.
///
/// Returns data with format "V{variant}-SEG-{segment}:TEST_SEGMENT_DATA" = 26 bytes prefix,
/// padded to ~200KB for realistic HLS testing.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_segment_data(variant: usize, segment: usize) -> Vec<u8> {
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");

    let target_size = 200_000;
    if data.len() < target_size {
        data.resize(target_size, 0xFF);
    }

    data
}

/// Master playlist with encrypted variant
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_master_playlist_encrypted() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0-encrypted.m3u8
"#
}

/// Media playlist with AES-128 encryption for testing
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn test_media_playlist_encrypted(_variant: usize) -> String {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:4
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-KEY:METHOD=AES-128,URI="../aes/key.bin",IV=0x00000000000000000000000000000000
#EXTINF:4.0,
../aes/seg0.bin
#EXT-X-ENDLIST
"#
    .to_string()
}
