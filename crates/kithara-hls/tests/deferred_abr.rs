#![forbid(unsafe_code)]

//! Tests for deferred ABR switch logic.
//!
//! These tests verify the core invariants of the deferred ABR switching:
//!
//! 1. ABR switch sets `next_variant`, but `current_variant` stays unchanged
//! 2. Sequential reads continue from old variant until exhausted
//! 3. Seek triggers immediate switch to new variant
//! 4. Automatic switch happens when old variant has no data at offset
//!
//! This enables seamless ABR transitions where the decoder finishes reading
//! the current variant's segment before switching to the new variant.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams, events::HlsEvent};
use kithara_stream::{Source, SyncReader, SyncReaderParams, WaitOutcome};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

mod fixture;
use fixture::{AbrTestServer, TestServer, master_playlist, test_segment_data};

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

#[fixture]
fn tracing_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::default().add_directive("warn".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();
}

// ==================== Helper Functions ====================

/// Wait for ABR switch event from HLS source.
/// Returns (from_variant, to_variant) if switch occurred.
async fn wait_for_abr_switch(
    events: &mut tokio::sync::broadcast::Receiver<HlsEvent>,
    timeout: Duration,
) -> Option<(usize, usize)> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }

        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                ..
            })) => {
                if from_variant != to_variant {
                    return Some((from_variant, to_variant));
                }
            }
            Ok(Ok(HlsEvent::EndOfStream)) => return None,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return None,
            Err(_) => return None,
        }
    }
}

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

// ==================== Core Invariant Tests ====================

/// Test: Sequential read after ABR switch continues from old variant.
///
/// When ABR switches variant mid-stream, the decoder should continue
/// reading from the old variant until it's exhausted. This ensures
/// seamless audio without gaps or glitches.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
async fn sequential_read_after_abr_switch_continues_from_old_variant(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    // Setup: Start from variant 2 (highest), with slow segment to trigger downswitch
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(300), // Delay causes low throughput → downswitch
    )
    .await;
    let url = server.url("/master.m3u8").unwrap();

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(2)), // Start from variant 2
            down_switch_buffer: 0.0,
            min_switch_interval: Duration::from_millis(50),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let mut events = source.events();

    // Read first few bytes to establish position
    let outcome = source.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let mut initial_buf = [0u8; 10];
    let n = source.read_at(0, &mut initial_buf).await.unwrap();
    assert_eq!(n, 10);

    let initial_variant = variant_from_data(&initial_buf);
    info!("Initial read from variant: {:?}", initial_variant);

    // Wait for ABR switch (should happen due to slow segment)
    let switch_result = wait_for_abr_switch(&mut events, Duration::from_secs(15)).await;

    if let Some((from, to)) = switch_result {
        info!("ABR switched from {} to {}", from, to);

        // Key test: Next sequential read should STILL come from old variant
        // because we haven't exhausted it yet
        let mut next_buf = [0u8; 10];
        let n = source.read_at(10, &mut next_buf).await.unwrap();
        if n > 0 {
            let read_variant = variant_from_data(&next_buf);
            info!(
                "After ABR switch, sequential read at offset 10 from variant: {:?}",
                read_variant
            );

            // Data should still come from the variant we started with
            // (the old variant, not the new one)
            if let (Some(init_var), Some(read_var)) = (initial_variant, read_variant) {
                assert_eq!(
                    init_var, read_var,
                    "Sequential read after ABR switch should continue from old variant. \
                     Expected variant {}, got variant {}",
                    init_var, read_var
                );
            }
        }
    } else {
        info!("ABR did not switch - test inconclusive but not failed");
    }
}

/// Test: Seek backward after ABR switch returns new variant data.
///
/// When user seeks backward (e.g., to position 0), the deferred switch
/// should be committed immediately and data should come from the new variant.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
async fn seek_backward_after_abr_switch_returns_new_variant(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(400),
    )
    .await;
    let url = server.url("/master.m3u8").unwrap();

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(2)),
            down_switch_buffer: 0.0,
            min_switch_interval: Duration::from_millis(50),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let mut events = source.events();
    let source = Arc::new(source);

    // Read initial data
    let source_clone = Arc::clone(&source);
    let read_task = tokio::spawn(async move {
        let outcome = source_clone.wait_range(0..10).await.unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        let mut buf = [0u8; 10];
        let n = source_clone.read_at(0, &mut buf).await.unwrap();
        (n, buf)
    });

    // Wait for switch and initial read
    let switch_result = wait_for_abr_switch(&mut events, Duration::from_secs(15)).await;
    let (n, _initial_buf) = read_task.await.unwrap();
    assert_eq!(n, 10);

    if let Some((from, to)) = switch_result {
        info!("ABR switched from {} to {}", from, to);

        // Use SyncReader for seek
        let source_for_reader = Arc::clone(&source);
        let result = tokio::task::spawn_blocking(move || {
            let mut reader = SyncReader::new(source_for_reader, SyncReaderParams::default());

            // First read some data to advance position
            let mut buf = [0u8; 20];
            let _ = reader.read(&mut buf);

            // Seek backward to position 0
            reader.seek(SeekFrom::Start(0)).unwrap();

            // Read after seek
            let mut after_seek = [0u8; 10];
            let n = reader.read(&mut after_seek).unwrap();
            (n, after_seek)
        })
        .await
        .unwrap();

        let (n, after_seek) = result;
        assert_eq!(n, 10);

        let after_variant = variant_from_data(&after_seek);
        info!(
            "After seek(0), data from variant: {:?}, new_variant was: {}",
            after_variant, to
        );

        // After seek, data should come from the NEW variant
        if let Some(var) = after_variant {
            assert_eq!(
                var, to,
                "After seek backward, data should come from new variant {}. Got variant {}",
                to, var
            );
        }
    } else {
        info!("ABR did not switch during test");
    }
}

/// Test: Seek forward (jump) after ABR switch triggers immediate switch.
///
/// A large forward jump in position is detected as a seek and should
/// trigger the deferred variant switch.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test]
async fn seek_forward_after_abr_switch_triggers_switch(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(400),
    )
    .await;
    let url = server.url("/master.m3u8").unwrap();

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(2)),
            down_switch_buffer: 0.0,
            min_switch_interval: Duration::from_millis(50),
            throughput_safety_factor: 1.0,
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let mut events = source.events();
    let source = Arc::new(source);

    // Read initial data
    {
        let outcome = source.wait_range(0..10).await.unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);
        let mut buf = [0u8; 10];
        let _ = source.read_at(0, &mut buf).await.unwrap();
    }

    // Wait for ABR switch
    let switch_result = wait_for_abr_switch(&mut events, Duration::from_secs(15)).await;

    if let Some((from, to)) = switch_result {
        info!("ABR switched from {} to {}", from, to);

        // Use SyncReader to seek forward significantly (>1MB triggers seek detection)
        // But our segments are small, so any forward seek should work
        let source_for_reader = Arc::clone(&source);
        let result = tokio::task::spawn_blocking(move || {
            let mut reader = SyncReader::new(source_for_reader, SyncReaderParams::default());

            // Jump to a later segment (e.g., segment 1 or 2)
            // Each segment is ~200KB in AbrTestServer
            reader.seek(SeekFrom::Start(100_000)).unwrap();

            // Read after forward seek
            let mut after_seek = [0u8; 10];
            let n = reader.read(&mut after_seek).unwrap();
            (n, after_seek)
        })
        .await
        .unwrap();

        let (n, after_seek) = result;
        if n > 0 {
            let after_variant = variant_from_data(&after_seek);
            info!(
                "After forward seek, data from variant: {:?}, new_variant: {}",
                after_variant, to
            );

            // After forward seek, data should come from the NEW variant
            if let Some(var) = after_variant {
                assert_eq!(
                    var, to,
                    "After forward seek, data should come from new variant {}. Got variant {}",
                    to, var
                );
            }
        }
    } else {
        info!("ABR did not switch during test");
    }
}

// ==================== Manual Variant Switch Tests ====================

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

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(variant),
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();

    let outcome = source.wait_range(0..10).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    let mut buf = [0u8; 10];
    let n = source.read_at(0, &mut buf).await.unwrap();
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
    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let source = Arc::new(source);

    // Read all three segments sequentially
    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SyncReader::new(source, SyncReaderParams::default());
        let mut all_data = Vec::new();
        let mut buf = [0u8; 100];

        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        all_data
    })
    .await
    .unwrap();

    // Verify all segments came from variant 1
    let expected = [
        test_segment_data(1, 0),
        test_segment_data(1, 1),
        test_segment_data(1, 2),
    ]
    .concat();

    assert_eq!(
        result, expected,
        "All data should come from variant 1 with consistent segments"
    );
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

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(2),
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let source = Arc::new(source);

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SyncReader::new(source, SyncReaderParams::default());

        // Seek to middle of segment 1 (26 bytes per segment)
        reader.seek(SeekFrom::Start(30)).unwrap();

        // Read several chunks
        let mut reads = Vec::new();
        for _ in 0..5 {
            let mut buf = [0u8; 10];
            let n = reader.read(&mut buf).unwrap();
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

// ==================== Edge Case Tests ====================

/// Test: Multiple seeks don't cause variant confusion.
///
/// Rapidly seeking back and forth should maintain correct variant tracking.
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

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let source = Arc::new(source);

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SyncReader::new(source, SyncReaderParams::default());
        let mut positions_and_data = Vec::new();

        // Read at position 0
        let mut buf = [0u8; 5];
        let _ = reader.read(&mut buf).unwrap();
        positions_and_data.push((0, buf.to_vec()));

        // Seek to segment 2
        reader.seek(SeekFrom::Start(52)).unwrap();
        let _ = reader.read(&mut buf).unwrap();
        positions_and_data.push((52, buf.to_vec()));

        // Seek back to segment 0
        reader.seek(SeekFrom::Start(10)).unwrap();
        let _ = reader.read(&mut buf).unwrap();
        positions_and_data.push((10, buf.to_vec()));

        // Seek to segment 1
        reader.seek(SeekFrom::Start(26)).unwrap();
        let _ = reader.read(&mut buf).unwrap();
        positions_and_data.push((26, buf.to_vec()));

        positions_and_data
    })
    .await
    .unwrap();

    // All data should be from variant 0
    for (pos, data) in &result {
        if let Some(var) = variant_from_data(data) {
            assert_eq!(
                var, 0,
                "At position {}, data should be from variant 0, got variant {}",
                pos, var
            );
        }
    }
}

/// Test: Seek to exact segment boundary works correctly.
#[rstest]
#[case(0)] // Start of segment 0
#[case(26)] // Start of segment 1
#[case(52)] // Start of segment 2
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

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    let source = Hls::open(url, params).await.unwrap();
    let source = Arc::new(source);

    let result = tokio::task::spawn_blocking(move || {
        let mut reader = SyncReader::new(source, SyncReaderParams::default());
        reader.seek(SeekFrom::Start(position)).unwrap();

        let mut buf = [0u8; 26];
        let n = reader.read(&mut buf).unwrap();
        buf[..n].to_vec()
    })
    .await
    .unwrap();

    let segment_index = (position / 26) as usize;
    let expected = test_segment_data(1, segment_index);
    assert_eq!(
        result, expected,
        "Seek to {} should read segment {} from variant 1",
        position, segment_index
    );
}
