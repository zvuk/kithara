//! Isolated tests for HLS driver (Stream<Hls> seek behavior).
//!
//! Tests:
//! - Driver-1: Seek works after all segments downloaded (playlist finished)
//! - Driver-2: ABR switch + seek backward

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use kithara::assets::StoreOptions;
use kithara::events::{Event, EventBus, HlsEvent};
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{
    TestServer,
    abr::{AbrTestServer, master_playlist},
};
use kithara_test_utils::{cancel_token, temp_dir, tracing_setup};

/// Driver-1: Verify that seek works AFTER all segments have been downloaded.
///
/// Scenario:
/// 1. Load all 3 segments from playlist (variant 0, fixed ABR)
/// 2. Read all data until EOF (segment stream finishes)
/// 3. Seek back to segment 1 start (offset `200_000`)
/// 4. Verify segment 1 data is readable
///
/// EXPECTED: seek is processed, segment data is read correctly
#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_driver_seek_after_playlist_finished(
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
        // Read ALL data until EOF
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        // Verify all 3 segments loaded (200KB each = 600KB total)
        assert!(
            all_data.len() >= 600_000,
            "Should read all 3 segments (~600KB), got {} bytes",
            all_data.len()
        );

        // CRITICAL: Seek to segment 1 AFTER playlist finished and EOF reached
        let pos = stream.seek(SeekFrom::Start(200_000)).unwrap();
        assert_eq!(pos, 200_000);

        // Read and verify segment 1 data
        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-1:",
            "After seek past EOF to segment 1, should read V0-SEG-1: prefix"
        );
    })
    .await
    .unwrap();
}

/// Driver-2: ABR switch + seek backward.
///
/// Scenario:
/// 1. Start with variant 0, ABR enabled (auto mode)
/// 2. Read data forward (ABR may switch variants based on throughput)
/// 3. Seek backward to beginning
/// 4. Verify data is readable and consistent from the start
///
/// This tests seek backward at the Stream<Hls> level with ABR active,
/// without the full decoder chain.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_driver_abr_seek_backward(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    let server = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_secs(2), // Slow segment0 to potentially trigger ABR
    )
    .await;

    let url = server.url("/master.m3u8").unwrap();

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_events(bus)
        .with_abr(AbrOptions {
            down_switch_buffer_secs: 0.5,
            min_buffer_for_up_switch_secs: 1.0,
            mode: AbrMode::Auto(Some(0)),
            throughput_safety_factor: 1.2,
            ..Default::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    // Track variant switches in background
    let variant_switches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let switches_clone = variant_switches.clone();

    tokio::spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            match ev {
                Event::Hls(HlsEvent::VariantApplied {
                    from_variant,
                    to_variant,
                    ..
                }) => {
                    info!("Variant switch: {} -> {}", from_variant, to_variant);
                    switches_clone
                        .lock()
                        .unwrap()
                        .push((from_variant, to_variant));
                }
                Event::Hls(HlsEvent::EndOfStream) => break,
                _ => {}
            }
        }
    });

    // Give ABR time to start downloading
    tokio::time::sleep(Duration::from_millis(100)).await;

    tokio::task::spawn_blocking(move || {
        // Read some data forward
        let mut first_read = vec![0u8; 50_000];
        let n1 = stream.read(&mut first_read).unwrap();
        assert!(n1 > 0, "Should read initial data");

        // Save first bytes for comparison
        let initial_prefix = first_read[..n1.min(9)].to_vec();

        // Seek backward to start
        let pos = stream.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(pos, 0, "Seek should return to position 0");

        // Read from beginning again
        let mut second_read = vec![0u8; 1000];
        let n2 = stream.read(&mut second_read).unwrap();
        assert!(n2 > 0, "Should read data after seeking to beginning");

        // Verify first bytes match (same data at position 0)
        let check_len = n2.min(initial_prefix.len());
        assert_eq!(
            &initial_prefix[..check_len],
            &second_read[..check_len],
            "Data at position 0 should be consistent before and after seek"
        );
    })
    .await
    .unwrap();

    // Log variant switches for debugging
    let switches = variant_switches.lock().unwrap();
    info!("Variant switches detected: {:?}", *switches);
}
