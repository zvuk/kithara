#![forbid(unsafe_code)]

//! Regression test for "seek past EOF" when actual segment sizes exceed metadata.
//!
//! Production scenario:
//!   1. Metadata via HEAD reports total = X bytes
//!   2. Actual segments on disk are slightly larger (e.g. HTTP auto-decompression)
//!   3. `expected_total_length` is set to X (HEAD-based)
//!   4. Symphonia/reader tries to seek to position > X
//!   5. Reader rejects seek as "seek past EOF"
//!
//! This test simulates the mismatch by having the server return
//! smaller Content-Length for HEAD than the actual GET body.

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use kithara::assets::StoreOptions;
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

use kithara_test_utils::{cancel_token, debug_tracing_setup, temp_dir};

// Constants

/// Actual segment body size returned by GET.
const ACTUAL_SEGMENT_SIZE: usize = 200_000;

/// Content-Length reported by HEAD (smaller, simulating compressed size).
/// ~800 bytes less per segment — matches real-world gzip/brotli overhead.
const HEAD_REPORTED_SIZE: usize = 199_200;

const NUM_SEGMENTS: usize = 3;

/// HEAD-based total across all segments (no init segments in this test).
const HEAD_TOTAL: u64 = (HEAD_REPORTED_SIZE * NUM_SEGMENTS) as u64; // 597_600

/// Actual total bytes that will be downloaded and cached.
const ACTUAL_TOTAL: u64 = (ACTUAL_SEGMENT_SIZE * NUM_SEGMENTS) as u64; // 600_000

// Test Server

fn segment_data(variant: usize, segment: usize) -> Vec<u8> {
    let prefix = format!("V{}-SEG-{}:", variant, segment);
    let mut data = prefix.into_bytes();
    data.extend(b"TEST_SEGMENT_DATA");
    if data.len() < ACTUAL_SEGMENT_SIZE {
        data.resize(ACTUAL_SEGMENT_SIZE, 0xFF);
    }
    data
}

fn media_playlist(variant: usize) -> String {
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

fn master_playlist() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=854x480,CODECS="avc1.42c01e,mp4a.40.2"
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1280x720,CODECS="avc1.42c01e,mp4a.40.2"
v1.m3u8
"#
}

/// HEAD handler: returns Content-Length smaller than actual body.
///
/// Simulates HTTP compression scenario where HEAD reports compressed size
/// but GET body is auto-decompressed by the HTTP client.
async fn segment_head_response() -> (StatusCode, HeaderMap) {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-length",
        HEAD_REPORTED_SIZE.to_string().parse().unwrap(),
    );
    (StatusCode::OK, headers)
}

/// Start a test HTTP server with mismatched HEAD/GET Content-Lengths.
async fn start_mismatch_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let app = Router::new()
        .route("/master.m3u8", get(|| async { master_playlist() }))
        .route("/v0.m3u8", get(|| async { media_playlist(0) }))
        .route("/v1.m3u8", get(|| async { media_playlist(1) }))
        // GET returns ACTUAL_SEGMENT_SIZE bytes; HEAD returns HEAD_REPORTED_SIZE
        .route(
            "/seg/v0_0.bin",
            get(|| async { segment_data(0, 0) }).head(segment_head_response),
        )
        .route(
            "/seg/v0_1.bin",
            get(|| async { segment_data(0, 1) }).head(segment_head_response),
        )
        .route(
            "/seg/v0_2.bin",
            get(|| async { segment_data(0, 2) }).head(segment_head_response),
        )
        .route(
            "/seg/v1_0.bin",
            get(|| async { segment_data(1, 0) }).head(segment_head_response),
        )
        .route(
            "/seg/v1_1.bin",
            get(|| async { segment_data(1, 1) }).head(segment_head_response),
        )
        .route(
            "/seg/v1_2.bin",
            get(|| async { segment_data(1, 2) }).head(segment_head_response),
        );

    let url: Url = format!("{}/master.m3u8", base_url).parse().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    url
}

// Tests

/// Seek to a position between HEAD-reported total and actual total must succeed.
///
/// Reproduces the production "seek past EOF" bug:
/// - HEAD total: `597_600` (3 x `199_200`)
/// - Actual total: `600_000` (3 x `200_000`)
/// - Seek to `598_000` is valid but fails if `expected_total_length` = HEAD total.
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seek_beyond_head_total_within_actual_total(
    _debug_tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let url = start_mismatch_server().await;

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Step 1: Read all data sequentially (downloads all 3 segments).
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        // Verify we got more data than HEAD reported.
        assert!(
            all_data.len() as u64 > HEAD_TOTAL,
            "Read {} bytes, expected more than HEAD total {}",
            all_data.len(),
            HEAD_TOTAL,
        );

        // Step 2: Seek to a position between HEAD total and actual total.
        // This position is valid (data exists) but would be rejected
        // if expected_total_length only reflects HEAD Content-Lengths.
        let seek_target = HEAD_TOTAL + 1; // 597_601
        let result = stream.seek(SeekFrom::Start(seek_target));

        assert!(
            result.is_ok(),
            "Seek to {} should succeed (HEAD total={}, actual total={}): {:?}",
            seek_target,
            HEAD_TOTAL,
            ACTUAL_TOTAL,
            result.err(),
        );

        // Step 3: Verify we can read at that position.
        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Should read data after seek to {}", seek_target);

        // Step 4: The data at this position should be 0xFF padding
        // (we're past the segment prefix but within the segment).
        assert_eq!(&buf[..n], &vec![0xFF; n][..], "Expected padding bytes");
    })
    .await
    .unwrap();
}
