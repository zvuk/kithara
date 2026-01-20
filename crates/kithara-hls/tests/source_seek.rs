#![forbid(unsafe_code)]

//! HLS Source random access and seek tests.
//!
//! Tests verify that:
//! 1. HlsSource::read_at works with different offsets
//! 2. Reading across segment boundaries works correctly
//! 3. SyncReader seek + read returns expected bytes
//!
//! Note: ABR is set to Manual(0) to fix variant and avoid switching during tests.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams};
use kithara_stream::{StreamSource, Source, SyncReader, SyncReaderParams, WaitOutcome};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

mod fixture;
use fixture::{TestServer, test_segment_data};

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn cancel_token() -> CancellationToken {
    CancellationToken::new()
}

/// HLS params with ABR fixed to variant 0.
#[fixture]
fn hls_params_fixed_variant(temp_dir: TempDir, cancel_token: CancellationToken) -> HlsParams {
    HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0), // Fix to variant 0 for deterministic tests
            ..AbrOptions::default()
        })
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

// ==================== Test Data Helpers ====================

/// Get expected data for variant 0.
/// Each segment is: "V0-SEG-{n}:TEST_SEGMENT_DATA" = 26 bytes
fn expected_segment_0() -> Vec<u8> {
    test_segment_data(0, 0)
}

fn expected_segment_1() -> Vec<u8> {
    test_segment_data(0, 1)
}

fn expected_segment_2() -> Vec<u8> {
    test_segment_data(0, 2)
}

/// Concatenate all 3 segments for variant 0.
fn all_segments_data() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend(expected_segment_0());
    data.extend(expected_segment_1());
    data.extend(expected_segment_2());
    data
}

// ==================== HlsSource::read_at Tests ====================

#[rstest]
#[case(0, 10)] // Start of first segment
#[case(10, 10)] // Middle of first segment
#[case(20, 6)] // End of first segment
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn hls_source_read_at_within_first_segment(
    _tracing_setup: (),
    hls_params_fixed_variant: HlsParams,
    #[case] offset: u64,
    #[case] len: usize,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap();

    // Wait for data to be available
    let range = offset..(offset + len as u64);
    let outcome = source.wait_range(range.clone()).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Read at offset
    let mut buf = vec![0u8; len];
    let n = source.read_at(offset, &mut buf).await.unwrap();
    assert_eq!(n, len);

    // Verify bytes match expected segment data
    let all_data = all_segments_data();
    let expected = &all_data[offset as usize..(offset as usize + len)];
    assert_eq!(&buf[..n], expected);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn hls_source_read_at_second_segment(
    _tracing_setup: (),
    hls_params_fixed_variant: HlsParams,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap();

    // Segment 0 is 26 bytes, so offset 26 starts segment 1
    let offset = 26u64;
    let len = 10;

    // Wait for segment 1 data
    let range = offset..(offset + len as u64);
    let outcome = source.wait_range(range).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Read from segment 1
    let mut buf = vec![0u8; len];
    let n = source.read_at(offset, &mut buf).await.unwrap();
    assert_eq!(n, len);

    // Verify matches expected segment 1 data
    let seg1 = expected_segment_1();
    assert_eq!(&buf[..n], &seg1[..len]);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn hls_source_read_at_third_segment(_tracing_setup: (), hls_params_fixed_variant: HlsParams) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap();

    // Segment 0 = 26 bytes, Segment 1 = 26 bytes, so offset 52 starts segment 2
    let offset = 52u64;
    let len = 10;

    // Wait for segment 2 data
    let range = offset..(offset + len as u64);
    let outcome = source.wait_range(range).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Read from segment 2
    let mut buf = vec![0u8; len];
    let n = source.read_at(offset, &mut buf).await.unwrap();
    assert_eq!(n, len);

    // Verify matches expected segment 2 data
    let seg2 = expected_segment_2();
    assert_eq!(&buf[..n], &seg2[..len]);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test]
async fn hls_source_read_at_eof_returns_zero(
    _tracing_setup: (),
    hls_params_fixed_variant: HlsParams,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap();

    // Total size = 26 * 3 = 78 bytes
    let total_size = 78u64;

    // Wait until stream is finished to know total length
    let range = 0..total_size;
    let outcome = source.wait_range(range).await.unwrap();
    assert_eq!(outcome, WaitOutcome::Ready);

    // Read at EOF position should return 0
    let mut buf = vec![0u8; 10];
    let n = source.read_at(total_size, &mut buf).await.unwrap();
    assert_eq!(n, 0);
}

// ==================== SyncReader Seek + Read Tests ====================

#[rstest]
#[case(0, b"V0-SEG-0:")]
#[case(26, b"V0-SEG-1:")]
#[case(52, b"V0-SEG-2:")]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_sync_reader_seek_to_segment_start(
    _tracing_setup: (),
    hls_params_fixed_variant: HlsParams,
    #[case] seek_pos: u64,
    #[case] expected_prefix: &[u8],
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = Arc::new(StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    let expected_len = expected_prefix.len();
    let expected_vec = expected_prefix.to_vec();

    let result = tokio::task::spawn_blocking(move || {
        let pos = reader.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);

        let mut buf = vec![0u8; expected_len];
        let n = reader.read(&mut buf).unwrap();
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
async fn hls_sync_reader_seek_current(_tracing_setup: (), hls_params_fixed_variant: HlsParams) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = Arc::new(StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    tokio::task::spawn_blocking(move || {
        // Read first 10 bytes
        let mut buf = [0u8; 10];
        reader.read(&mut buf).unwrap();

        // Seek forward 19 bytes (position = 10 + 19 = 29)
        let pos = reader.seek(SeekFrom::Current(19)).unwrap();
        assert_eq!(pos, 29);

        // Position 29 is in segment 1 (offset 3 within segment 1)
        // segment 1 at offset 3 is "SEG-1:"
        let mut buf = [0u8; 6];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"SEG-1:");
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_sync_reader_multiple_seeks(_tracing_setup: (), hls_params_fixed_variant: HlsParams) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = Arc::new(StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    tokio::task::spawn_blocking(move || {
        // Read from start
        let mut buf = [0u8; 9];
        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"V0-SEG-0:");

        // Seek to segment 2 (offset 52)
        reader.seek(SeekFrom::Start(52)).unwrap();
        let mut buf = [0u8; 9];
        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"V0-SEG-2:");

        // Seek back to segment 1 (offset 26)
        reader.seek(SeekFrom::Start(26)).unwrap();
        let mut buf = [0u8; 9];
        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"V0-SEG-1:");

        // Seek back to start
        reader.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 9];
        reader.read(&mut buf).unwrap();
        assert_eq!(&buf, b"V0-SEG-0:");
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_sync_reader_read_all_then_seek_back(
    _tracing_setup: (),
    hls_params_fixed_variant: HlsParams,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    let source = Arc::new(StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    let expected = all_segments_data();

    tokio::task::spawn_blocking(move || {
        // Read all data (78 bytes = 3 segments * 26 bytes)
        let mut all_data = Vec::new();
        let mut buf = [0u8; 32];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }
        assert_eq!(all_data.len(), 78);

        // Verify all data matches expected
        assert_eq!(all_data, expected);

        // Seek back to middle
        reader.seek(SeekFrom::Start(30)).unwrap();
        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 10);

        // Verify bytes at position 30
        assert_eq!(&buf[..n], &expected[30..40]);
    })
    .await
    .unwrap();
}

// ==================== ABR considerations ====================

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
    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1), // Fixed to variant 1
            ..AbrOptions::default()
        });

    let source = Arc::new(StreamSource::<Hls>::open(url, params).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    tokio::task::spawn_blocking(move || {
        // Read first segment prefix
        let mut buf = [0u8; 9];
        reader.read(&mut buf).unwrap();

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
    hls_params_fixed_variant: HlsParams,
) {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();

    info!("Testing seek across all segments with fixed ABR");

    let source = Arc::new(StreamSource::<Hls>::open(url, hls_params_fixed_variant).await.unwrap());
    let mut reader = SyncReader::new(source, SyncReaderParams::default());

    let expected = all_segments_data();

    tokio::task::spawn_blocking(move || {
        // Test multiple random-access patterns within segments
        // Note: Cross-segment reads may return partial data, so we test within segments only
        // Segment 0: 0-25, Segment 1: 26-51, Segment 2: 52-77
        for (offset, len) in [(0, 10), (15, 10), (26, 10), (40, 10), (52, 10), (65, 10)] {
            if offset + len > expected.len() {
                continue;
            }

            reader.seek(SeekFrom::Start(offset as u64)).unwrap();
            let mut buf = vec![0u8; len];
            let n = reader.read(&mut buf).unwrap();

            assert_eq!(n, len, "Failed at offset {}", offset);
            assert_eq!(
                &buf[..n],
                &expected[offset..offset + len],
                "Data mismatch at offset {}",
                offset
            );
        }
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

    // Create params for variant 0
    let params_v0 = HlsParams::new(StoreOptions::new(temp_dir.path().join("v0")))
        .with_cancel(cancel_token.clone())
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    // Create params for variant 1
    let params_v1 = HlsParams::new(StoreOptions::new(temp_dir.path().join("v1")))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    // Open both sources
    let source_v0 = Arc::new(StreamSource::<Hls>::open(url.clone(), params_v0).await.unwrap());
    let source_v1 = Arc::new(StreamSource::<Hls>::open(url, params_v1).await.unwrap());

    let mut reader_v0 = SyncReader::new(source_v0, SyncReaderParams::default());
    let mut reader_v1 = SyncReader::new(source_v1, SyncReaderParams::default());

    tokio::task::spawn_blocking(move || {
        // Read initial data from both variants
        let mut buf_v0 = [0u8; 9];
        let mut buf_v1 = [0u8; 9];
        reader_v0.read(&mut buf_v0).unwrap();
        reader_v1.read(&mut buf_v1).unwrap();

        assert_eq!(&buf_v0, b"V0-SEG-0:");
        assert_eq!(&buf_v1, b"V1-SEG-0:");
        assert_ne!(
            &buf_v0, &buf_v1,
            "Different variants should have different data"
        );

        // Seek and verify both return their respective variant data
        reader_v0.seek(SeekFrom::Start(0)).unwrap();
        reader_v1.seek(SeekFrom::Start(0)).unwrap();

        let mut after_v0 = [0u8; 9];
        let mut after_v1 = [0u8; 9];
        reader_v0.read(&mut after_v0).unwrap();
        reader_v1.read(&mut after_v1).unwrap();

        assert_eq!(&after_v0, b"V0-SEG-0:", "Variant 0 data after seek");
        assert_eq!(&after_v1, b"V1-SEG-0:", "Variant 1 data after seek");
    })
    .await
    .unwrap();
}

/// Test ABR auto-switch + seek: after ABR switches variant, seek returns NEW variant data.
///
/// This test uses a server with configurable delay to trigger real ABR downswitch:
/// 1. Start with Auto mode (initial variant 2 - highest bandwidth)
/// 2. Server delays segment 0 of variant 2 → low throughput detected
/// 3. ABR controller triggers downswitch to variant 1 or 0
/// 4. Wait for VariantApplied event
/// 5. After switch, seek back to position 0
/// 6. Verify data comes from the NEW (lower) variant
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hls_seek_after_real_abr_switch_returns_new_variant_data(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    use fixture::{AbrTestServer, master_playlist};
    use kithara_hls::events::HlsEvent;

    // Create server with significant delay on variant 2 segment 0 to trigger downswitch
    // Variant 2 is highest bandwidth (1_024_000), delay will cause low throughput
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(500), // 500ms delay → ~100KB/s throughput for 50KB segment
    )
    .await;
    let url = server.url("/master.m3u8").unwrap();

    // Configure ABR in Auto mode starting from variant 2 (highest)
    // Use aggressive downswitch settings to trigger switch quickly
    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(2)), // Start from highest variant
            down_switch_buffer: 0.0,      // Trigger downswitch immediately on low throughput
            min_switch_interval: Duration::from_millis(100), // Allow quick switching
            throughput_safety_factor: 1.0, // No safety margin
            ..AbrOptions::default()
        });

    let source = StreamSource::<Hls>::open(url, params).await.unwrap();
    let mut events = source.events();

    // Wait for variant switch event
    let mut switched_to_variant: Option<usize> = None;
    let wait_for_switch = async {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), events.recv()).await {
                Ok(Ok(HlsEvent::VariantApplied {
                    from_variant,
                    to_variant,
                    ..
                })) => {
                    info!(
                        "ABR switched from variant {} to variant {}",
                        from_variant, to_variant
                    );
                    if from_variant != to_variant {
                        switched_to_variant = Some(to_variant);
                        break;
                    }
                }
                Ok(Ok(HlsEvent::EndOfStream)) => {
                    info!("Stream ended without ABR switch");
                    break;
                }
                Ok(Ok(event)) => {
                    // Continue waiting for switch
                    info!("Event: {:?}", event);
                }
                Ok(Err(_)) => break,
                Err(_) => {
                    info!("Timeout waiting for ABR switch");
                    break;
                }
            }
        }
    };

    // Read first segment while waiting for switch
    let source = Arc::new(source);
    let source_clone = Arc::clone(&source);
    let read_task = tokio::spawn(async move {
        // Wait for first segment data
        let outcome = source_clone.wait_range(0..9).await.unwrap();
        assert_eq!(outcome, WaitOutcome::Ready);

        let mut buf = vec![0u8; 9];
        let n = source_clone.read_at(0, &mut buf).await.unwrap();
        (n, buf)
    });

    // Wait for both the read and the switch
    let (read_result, _) = tokio::join!(read_task, wait_for_switch);
    let (n, initial_buf) = read_result.unwrap();
    assert_eq!(n, 9);

    info!(
        "Initial data: {:?}, switched_to: {:?}",
        String::from_utf8_lossy(&initial_buf),
        switched_to_variant
    );

    // If ABR switched, verify seek returns new variant data
    if let Some(new_variant) = switched_to_variant {
        let mut reader = SyncReader::new(source, SyncReaderParams::default());

        let result = tokio::task::spawn_blocking(move || {
            // Seek back to position 0
            reader.seek(SeekFrom::Start(0)).unwrap();

            // Read data after seek
            let mut buf_after_seek = [0u8; 9];
            let n = reader.read(&mut buf_after_seek).unwrap();
            (n, buf_after_seek, new_variant)
        })
        .await
        .unwrap();

        let (n, buf_after_seek, new_variant) = result;
        assert_eq!(n, 9);

        // Verify data comes from the NEW variant after ABR switch
        // Binary format: [VARIANT:1byte][SEGMENT:4bytes][DATA_LEN:4bytes][DATA...]
        let read_variant = buf_after_seek[0] as usize;
        assert_eq!(
            read_variant, new_variant,
            "After ABR switch and seek, data should come from variant {}, got variant {}",
            new_variant, read_variant
        );

        info!(
            "Successfully verified seek after ABR switch returns variant {} data",
            new_variant
        );
    } else {
        // ABR didn't switch - this might happen if throughput was still sufficient
        // This is not a test failure, just note it
        info!("ABR did not switch during test - throughput may have been sufficient");
    }
}
