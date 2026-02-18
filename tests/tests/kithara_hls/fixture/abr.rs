//! ABR (Adaptive Bitrate) test server
//!
//! Provides `AbrTestServer` for testing throughput-based bitrate switching.

use std::time::Duration;

use axum::{Router, routing::get};
use kithara_test_utils::TestHttpServer;
use rstest::fixture;
use url::Url;

use super::{HlsResult, crypto::init_data};

/// ABR test server with configurable delays and bitrates
pub(crate) struct AbrTestServer {
    http: TestHttpServer,
}

impl AbrTestServer {
    pub(crate) async fn new(master_playlist: String, init: bool, segment0_delay: Duration) -> Self {
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

        let http = TestHttpServer::new(app).await;

        Self { http }
    }

    #[expect(
        clippy::result_large_err,
        reason = "test-only code, ergonomics over size"
    )]
    pub(crate) fn url(&self, path: &str) -> HlsResult<Url> {
        Ok(self.http.url(path))
    }
}

/// Generate master playlist with custom bitrates
pub(crate) fn master_playlist(v0_bw: u64, v1_bw: u64, v2_bw: u64) -> String {
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

/// Generate media playlist for variant
pub(crate) fn media_playlist(variant: usize, init: bool) -> String {
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
pub(crate) async fn segment_data(
    variant: usize,
    segment: usize,
    delay: Duration,
    total_len: usize,
) -> Vec<u8> {
    if delay != Duration::ZERO {
        tokio::time::sleep(delay).await;
    }

    // Binary format: [VARIANT:1byte][SEGMENT:4bytes][DATA_LEN:4bytes][DATA:DATA_LEN bytes]
    let mut data = Vec::new();

    // VARIANT (1 byte)
    data.push(variant as u8);

    // SEGMENT (4 bytes, big-endian)
    data.extend(&(segment as u32).to_be_bytes());

    // Calculate DATA_LEN (total_len - header size)
    let header_size = 1 + 4 + 4; // variant + segment + data_len
    let data_len = total_len.saturating_sub(header_size);

    // DATA_LEN (4 bytes, big-endian)
    data.extend(&(data_len as u32).to_be_bytes());

    // DATA (padding with 'A')
    data.extend(std::iter::repeat_n(b'A', data_len));

    data
}

/// Fixture: default ABR test server
#[fixture]
pub(crate) async fn abr_server_default() -> AbrTestServer {
    AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(1),
    )
    .await
}
