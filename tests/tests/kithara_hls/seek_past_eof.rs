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

use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use fixture::scalable_server::{HlsTestServer, HlsTestServerConfig};
use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::{TestTempDir, cancel_token, debug_tracing_setup, temp_dir};
use tokio_util::sync::CancellationToken;

use super::fixture;

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

// Tests

/// Seek to a position between HEAD-reported total and actual total must succeed.
///
/// Reproduces the production "seek past EOF" bug:
/// - HEAD total: `597_600` (3 x `199_200`)
/// - Actual total: `600_000` (3 x `200_000`)
/// - Seek to `598_000` is valid but fails if `expected_total_length` = HEAD total.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(15)))]
async fn seek_beyond_head_total_within_actual_total(
    _debug_tracing_setup: (),
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: NUM_SEGMENTS,
        segment_size: ACTUAL_SEGMENT_SIZE,
        head_reported_segment_size: Some(HEAD_REPORTED_SIZE),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    kithara_platform::spawn_blocking(move || {
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
