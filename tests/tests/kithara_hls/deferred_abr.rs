#![forbid(unsafe_code)]

//! Tests for deferred ABR switch logic.
//!
//! These tests verify the core invariants of variant switching:
//!
//! 1. Manual variant selection returns correct data
//! 2. Sequential reads maintain variant consistency
//! 3. Seek returns data from correct variant
//! 4. Multiple seeks maintain correct variant tracking

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use fixture::TestServer;
use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture;
use crate::common::fixtures::{cancel_token, temp_dir, tracing_setup};

// Helper Functions

/// Get variant from data by parsing prefix "V{n}-SEG-".
fn variant_from_data(data: &[u8]) -> Option<usize> {
    if data.len() < 2 || data[0] != b'V' {
        return None;
    }
    let digit = data[1];
    if digit.is_ascii_digit() {
        Some((digit - b'0') as usize)
    } else {
        None
    }
}

// Manual Variant Switch Tests

/// Test: Manual variant switch with fixed ABR, verify data comes from correct variant.
///
/// This tests the basic variant selection without ABR auto-switching.
#[rstest]
#[case(0)]
#[case(1)]
#[case(2)]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn manual_variant_returns_correct_data(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] variant: usize,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(variant),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    let result = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 10];
        let n = stream.read(&mut buf).unwrap();
        (n, buf)
    })
    .await
    .unwrap();

    let (n, buf) = result;
    assert!(n > 0);

    let read_variant = variant_from_data(&buf);
    assert_eq!(
        read_variant,
        Some(variant),
        "Data should come from variant {}. Got: {:?}",
        variant,
        String::from_utf8_lossy(&buf)
    );
}

/// Test: Reading across segment boundaries maintains variant consistency.
///
/// When reading sequentially across multiple segments, all data should
/// come from the same variant (no unexpected switches).
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test]
async fn sequential_read_across_segments_maintains_variant(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    // Fix to variant 1 to avoid any ABR switching
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    // Read all three segments sequentially
    // Use 64KB buffer to avoid async lock overhead per small read
    let result = tokio::task::spawn_blocking(move || {
        let mut all_data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut read_count = 0;

        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            read_count += 1;
            all_data.extend_from_slice(&buf[..n]);

            // Safety limit to prevent infinite loop
            if read_count > 10000 {
                panic!(
                    "Read loop exceeded 10000 iterations, likely infinite loop. Total bytes: {}",
                    all_data.len()
                );
            }
        }

        info!(
            "Completed reading {} bytes in {} iterations",
            all_data.len(),
            read_count
        );
        all_data
    })
    .await
    .unwrap();

    // Verify we read substantial amount of data (at least 500KB for 3 segments)
    assert!(
        result.len() > 500_000,
        "Should read substantial data, got {} bytes",
        result.len()
    );

    // Verify all data starts with variant 1 segment 0 prefix
    assert!(
        result.starts_with(b"V1-SEG-0:"),
        "Data should start with V1-SEG-0:"
    );

    info!("Read {} bytes total from variant 1", result.len());
}

/// Test: After seek, variant is maintained for subsequent sequential reads.
///
/// Once we seek and commit to a variant, sequential reads should
/// continue from that variant.
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test]
async fn after_seek_sequential_reads_maintain_variant(
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
            mode: AbrMode::Manual(2),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    let result = tokio::task::spawn_blocking(move || {
        // Seek to middle of segment 1 (200KB per segment)
        stream.seek(SeekFrom::Start(200_100)).unwrap();

        // Read several chunks
        let mut reads = Vec::new();
        for _ in 0..5 {
            let mut buf = [0u8; 10];
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            reads.push(buf[..n].to_vec());
        }

        reads
    })
    .await
    .unwrap();

    // All reads should be from variant 2
    for (i, data) in result.iter().enumerate() {
        if let Some(var) = variant_from_data(data) {
            assert_eq!(
                var, 2,
                "Read {} should be from variant 2, got variant {}",
                i, var
            );
        }
    }
}

// Edge Case Tests

/// Test: Multiple seeks don't cause variant confusion.
///
/// Rapidly seeking back and forth should maintain correct variant tracking.
/// Note: We first read all data to ensure segments are fetched, then seek.
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test]
async fn multiple_seeks_maintain_correct_variant(
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

    let result = tokio::task::spawn_blocking(move || {
        // First, read all data to ensure all segments are fetched
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }
        let total_len = all_data.len();
        info!("Read {} bytes total before seeking", total_len);

        let mut positions_and_data = Vec::new();

        // Seek back to start and verify
        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        let _ = stream.read(&mut buf).unwrap();
        positions_and_data.push((0, buf.to_vec()));

        // Seek to middle of stream (within segment 1)
        let mid_pos = 200_000u64;
        stream.seek(SeekFrom::Start(mid_pos)).unwrap();
        let _ = stream.read(&mut buf).unwrap();
        positions_and_data.push((mid_pos, buf.to_vec()));

        // Seek back to start
        stream.seek(SeekFrom::Start(0)).unwrap();
        let _ = stream.read(&mut buf).unwrap();
        positions_and_data.push((0, buf.to_vec()));

        // Seek to position 100 (within segment 0)
        stream.seek(SeekFrom::Start(100)).unwrap();
        let mut small_buf = [0u8; 6];
        let _ = stream.read(&mut small_buf).unwrap();
        positions_and_data.push((100, small_buf.to_vec()));

        positions_and_data
    })
    .await
    .unwrap();

    // Verify data from correct positions
    // Position 0: should read "V0-SEG-0:"
    assert_eq!(
        &result[0].1[..9],
        b"V0-SEG-0:",
        "Position 0 should read segment 0 prefix"
    );

    // Position 200,000: should read "V0-SEG-1:"
    assert_eq!(
        &result[1].1[..9],
        b"V0-SEG-1:",
        "Position 200000 should read segment 1 prefix"
    );

    // Position 0 again: should read "V0-SEG-0:"
    assert_eq!(
        &result[2].1[..9],
        b"V0-SEG-0:",
        "Position 0 (again) should read segment 0 prefix"
    );

    // Position 100: should be within padding (0xFF bytes)
    assert_eq!(
        &result[3].1, &[0xFF; 6],
        "Position 100 should be padding bytes"
    );
}

/// Test: Seek to exact segment boundary reads correct segment prefix.
/// Note: With 200KB segments, we only read the first 26 bytes to verify the segment.
#[rstest]
#[case(0)] // Start of segment 0
#[case(200_000)] // Start of segment 1
#[case(400_000)] // Start of segment 2
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn seek_to_segment_boundary_reads_correct_segment(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
    #[case] position: u64,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    let result = tokio::task::spawn_blocking(move || {
        stream.seek(SeekFrom::Start(position)).unwrap();

        // Read first 26 bytes (the meaningful prefix before padding)
        let mut buf = [0u8; 26];
        let n = stream.read(&mut buf).unwrap();
        buf[..n].to_vec()
    })
    .await
    .unwrap();

    let segment_index = (position / 200_000) as usize;
    // Expected prefix: "V1-SEG-{n}:TEST_SEGMENT_DATA" (26 bytes)
    let expected_prefix = format!("V1-SEG-{}:TEST_SEGMENT_DATA", segment_index);
    assert_eq!(
        result,
        expected_prefix.as_bytes(),
        "Seek to {} should read segment {} prefix from variant 1",
        position,
        segment_index
    );
}
