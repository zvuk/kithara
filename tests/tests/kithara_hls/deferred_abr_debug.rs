#![forbid(unsafe_code)]

//! Diagnostic test for `sequential_read_across_segments_maintains_variant`

use std::{io::Read, time::Duration};

use fixture::TestServer;
use kithara::assets::StoreOptions;
use kithara::events::EventBus;
use kithara::hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara::stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture;
use kithara_test_utils::{cancel_token, debug_tracing_setup, temp_dir};

/// Diagnostic version with detailed logging and safety limits
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test]
async fn debug_sequential_read(
    _debug_tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    info!("=== Starting debug_sequential_read test ===");

    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();
    info!("Test server URL: {}", url);

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        })
        .with_events(bus);

    info!("Opening HLS stream...");
    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    // Spawn event listener
    let events_handle = tokio::spawn(async move {
        while let Ok(event) = events_rx.recv().await {
            info!("HLS Event: {:?}", event);
        }
    });

    // Read with detailed logging
    // Use 64KB buffer to avoid async lock overhead per small read
    info!("Starting blocking read task...");
    let result = tokio::task::spawn_blocking(move || {
        info!("Inside blocking task, starting read");
        let mut all_data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut read_count = 0;
        let mut total_bytes = 0;

        info!("Starting read loop...");
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                info!("Read returned 0 (EOF), breaking loop");
                break;
            }

            read_count += 1;
            total_bytes += n;
            all_data.extend_from_slice(&buf[..n]);

            if read_count % 100 == 0 {
                info!(
                    "Progress: {} reads, {} bytes total",
                    read_count, total_bytes
                );
            }

            // Safety limit
            if read_count > 10000 {
                panic!(
                    "Read loop exceeded 10000 iterations. Total bytes: {}, likely infinite loop",
                    all_data.len()
                );
            }
        }

        info!(
            "Read loop completed: {} bytes in {} reads",
            all_data.len(),
            read_count
        );
        all_data
    })
    .await
    .unwrap();

    info!("Blocking task completed, received {} bytes", result.len());

    // Verify we read substantial amount of data (at least 500KB for 3 segments)
    assert!(
        result.len() > 500_000,
        "Should read substantial data, got {} bytes",
        result.len()
    );

    // Verify data starts with expected variant 1 segment 0 prefix
    assert!(
        result.starts_with(b"V1-SEG-0:"),
        "Data should start with V1-SEG-0: prefix"
    );

    info!("Read {} bytes total from variant 1", result.len());

    info!("Test passed!");

    // Cancel to shutdown cleanly
    drop(events_handle);
}
