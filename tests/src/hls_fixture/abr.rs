//! ABR (Adaptive Bitrate) test server
//!
//! Provides `AbrTestServer` for testing throughput-based bitrate switching.
//! On native: in-process axum server. On WASM: delegates to external fixture server.

// ── Native implementation ──────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::iter;

    use axum::{Router, routing::get};
    use kithara_platform::time::{Duration, sleep};
    use kithara_test_utils::TestHttpServer;
    use url::Url;

    use super::super::{HlsResult, crypto::init_data};

    /// ABR test server with configurable delays and bitrates
    pub struct AbrTestServer {
        http: TestHttpServer,
    }

    impl AbrTestServer {
        pub async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
            let master = master_playlist;

            let app = Router::new()
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

            let http = TestHttpServer::new(app).await;

            Self { http }
        }

        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(self.http.url(path))
        }
    }

    /// Generate media playlist for variant
    fn media_playlist(variant: usize, init: bool) -> String {
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

    /// Generate segment data with configurable delay and size
    async fn segment_data(
        variant: usize,
        segment: usize,
        delay: Duration,
        total_len: usize,
    ) -> Vec<u8> {
        if delay != Duration::ZERO {
            sleep(delay).await;
        }

        let mut data = Vec::new();
        data.push(variant as u8);
        data.extend(&(segment as u32).to_be_bytes());
        let header_size = 1 + 4 + 4;
        let data_len = total_len.saturating_sub(header_size);
        data.extend(&(data_len as u32).to_be_bytes());
        data.extend(iter::repeat_n(b'A', data_len));
        data
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::AbrTestServer;

// ── WASM implementation ────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::time::Duration;

    use kithara_test_utils::{fixture_client, fixture_protocol::AbrSessionConfig, join_server_url};
    use url::Url;

    use super::super::HlsResult;

    /// Remote ABR test server backed by fixture server session.
    pub struct AbrTestServer {
        session_id: String,
        base_url: Url,
    }

    impl AbrTestServer {
        pub async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
            let config = AbrSessionConfig {
                master_playlist,
                has_init: init,
                segment0_delay_ms: segment0_delay.as_millis() as u64,
            };
            let resp = fixture_client::create_abr_session(&config).await;
            Self {
                session_id: resp.session_id,
                base_url: resp.base_url.parse().unwrap(),
            }
        }

        #[expect(
            clippy::result_large_err,
            reason = "test-only code, ergonomics over size"
        )]
        pub fn url(&self, path: &str) -> HlsResult<Url> {
            Ok(join_server_url(&self.base_url, path))
        }
    }

    impl Drop for AbrTestServer {
        fn drop(&mut self) {
            let id = self.session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                fixture_client::delete_session(&id).await;
            });
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::AbrTestServer;

// ── Shared helpers (cross-platform) ────────────────────────────────

/// Generate master playlist with custom bitrates
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
