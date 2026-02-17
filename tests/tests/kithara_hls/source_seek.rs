#![forbid(unsafe_code)]

//! HLS Stream seek tests.
//!
//! Tests verify that:
//! 1. Stream<Hls> read + seek works with different offsets
//! 2. Reading across segment boundaries works correctly
//! 3. Seek + read returns expected bytes
//!
//! Note: ABR is set to Manual(0) to fix variant and avoid switching during tests.

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use fixture::TestServer;
use kithara::assets::StoreOptions;
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture;
use kithara_test_utils::{cancel_token, temp_dir, tracing_setup};

// Test Data Helpers

/// Segment size in bytes (test fixture pads to 200KB).
#[expect(dead_code, reason = "documents segment size for test reference")]
const SEGMENT_SIZE: u64 = 200_000;

// Stream<Hls> Seek + Read Tests

#[rstest]
#[case(0, b"V0-SEG-0:")] // Start of segment 0
#[case(200_000, b"V0-SEG-1:")] // Start of segment 1
#[case(400_000, b"V0-SEG-2:")] // Start of segment 2
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_stream_seek_to_segment_start(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] seek_pos: u64,
    #[case] expected_prefix: &[u8],
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0), // Fix to variant 0 for deterministic tests
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    let expected_len = expected_prefix.len();
    let expected_vec = expected_prefix.to_vec();

    let result = tokio::task::spawn_blocking(move || {
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);

        let mut buf = vec![0u8; expected_len];
        let n = stream.read(&mut buf).unwrap();
        (n, buf)
    })
    .await
    .unwrap();

    assert_eq!(result.0, expected_len);
    assert_eq!(&result.1[..result.0], &expected_vec[..]);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_stream_seek_current(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read first 10 bytes
        let mut buf = [0u8; 10];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 10);

        // Seek forward to position 29 (offset 3 within segment 0 prefix)
        // Position 29 is at byte 29 of segment 0 (still within the 0xFF padding)
        let pos = stream.seek(SeekFrom::Current(19)).unwrap();
        assert_eq!(pos, 29);

        // Position 29 is still in segment 0 (200KB per segment)
        // At byte 29 we're past "V0-SEG-0:TEST_SEGMENT_DATA" (26 bytes), so we read 0xFF padding
        let mut buf = [0u8; 6];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        // After the 26-byte prefix, rest is 0xFF padding
        assert_eq!(&buf, &[0xFF; 6]);
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_stream_multiple_seeks(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read from start
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:");

        // Seek back to start
        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:", "After seek to 0, should read segment 0");

        // Seek forward some amount within segment 0
        stream.seek(SeekFrom::Start(100)).unwrap();
        let mut buf = [0u8; 6];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        // At position 100, we're past the 26-byte prefix, so we read 0xFF padding
        assert_eq!(&buf, &[0xFF; 6], "Position 100 should be padding bytes");
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_stream_read_all_then_seek_back(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read all data
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024]; // 64KB buffer for efficiency
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        // Should read substantial amount (at least 500KB for 3 segments)
        assert!(
            all_data.len() > 500_000,
            "Should read substantial data, got {} bytes",
            all_data.len()
        );

        // Verify first segment starts with expected prefix
        assert!(
            all_data.starts_with(b"V0-SEG-0:"),
            "Data should start with V0-SEG-0:"
        );

        // Seek back to start and verify
        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-0:",
            "After seek to 0, should read segment 0 prefix"
        );
    })
    .await
    .unwrap();
}

// ABR considerations

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_with_manual_abr_uses_fixed_variant(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    // Use variant 1 instead of 0
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1), // Fixed to variant 1
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read first segment prefix
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);

        // Should be variant 1 data
        assert_eq!(&buf, b"V1-SEG-0:");
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_seek_across_all_segments_with_fixed_abr(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    info!("Testing seek across all segments with fixed ABR");

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Test seeking within segment 0 and verifying data
        // Segment 0 starts with "V0-SEG-0:TEST_SEGMENT_DATA" (26 bytes) then 0xFF padding

        // Read from start
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(&buf, b"V0-SEG-0:", "Start should be segment 0 prefix");

        // Seek to position 100 (within padding) and verify
        stream.seek(SeekFrom::Start(100)).unwrap();
        let mut buf = [0u8; 10];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 10);
        assert_eq!(&buf, &[0xFF; 10], "Position 100 should be padding");

        // Seek back to start and verify again
        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-0:",
            "After seek back, should still be segment 0"
        );
    })
    .await
    .unwrap();
}

/// Test that demonstrates ABR switch + seek behavior with manual variant selection.
///
/// This test shows that different variants produce different data at the same positions,
/// which is the foundation for ABR switch + seek correctness.
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_seek_different_variants_return_different_data(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    // Create config for variant 0
    let config_v0 = HlsConfig::new(url.clone())
        .with_store(StoreOptions::new(temp_dir.path().join("v0")))
        .with_cancel(cancel_token.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    // Create config for variant 1
    let config_v1 = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path().join("v1")))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    // Open both streams
    let mut stream_v0 = Stream::<Hls>::new(config_v0).await.unwrap();
    let mut stream_v1 = Stream::<Hls>::new(config_v1).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read initial data from both variants
        let mut buf_v0 = [0u8; 9];
        let mut buf_v1 = [0u8; 9];
        let n0 = stream_v0.read(&mut buf_v0).unwrap();
        let n1 = stream_v1.read(&mut buf_v1).unwrap();
        assert_eq!(n0, 9);
        assert_eq!(n1, 9);

        assert_eq!(&buf_v0, b"V0-SEG-0:");
        assert_eq!(&buf_v1, b"V1-SEG-0:");
        assert_ne!(
            &buf_v0, &buf_v1,
            "Different variants should have different data"
        );

        // Seek and verify both return their respective variant data
        stream_v0.seek(SeekFrom::Start(0)).unwrap();
        stream_v1.seek(SeekFrom::Start(0)).unwrap();

        let mut after_v0 = [0u8; 9];
        let mut after_v1 = [0u8; 9];
        let n0 = stream_v0.read(&mut after_v0).unwrap();
        let n1 = stream_v1.read(&mut after_v1).unwrap();
        assert_eq!(n0, 9);
        assert_eq!(n1, 9);

        assert_eq!(&after_v0, b"V0-SEG-0:", "Variant 0 data after seek");
        assert_eq!(&after_v1, b"V1-SEG-0:", "Variant 1 data after seek");
    })
    .await
    .unwrap();
}
