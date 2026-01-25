#![forbid(unsafe_code)]

//! Diagnostic test for sequential_read_across_segments_maintains_variant

use std::{io::Read, sync::Arc, time::Duration};

use fixture::{TestServer, test_segment_data};
use kithara_assets::StoreOptions;
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsParams, events::HlsEvent};
use kithara_stream::{Source, StreamSource, SyncReader, SyncReaderParams};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture;

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
            tracing_subscriber::EnvFilter::default()
                .add_directive("kithara_hls=debug".parse().unwrap())
                .add_directive("kithara_stream=debug".parse().unwrap())
                .add_directive("kithara_worker=debug".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();
}

/// Diagnostic version with detailed logging and safety limits
#[rstest]
#[timeout(Duration::from_secs(15))]
#[tokio::test]
async fn debug_sequential_read(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) {
    info!("=== Starting debug_sequential_read test ===");

    let server = TestServer::new().await;
    let url = server.url("/master.m3u8").unwrap();
    info!("Test server URL: {}", url);

    let params = HlsParams::new(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(1),
            ..AbrOptions::default()
        });

    info!("Opening HLS source...");
    let source = StreamSource::<Hls>::open(url, params).await.unwrap();
    let mut events = source.events();
    let source = Arc::new(source);

    info!("Source len: {:?}", source.len());

    // Spawn event listener
    let events_handle = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            info!("HLS Event: {:?}", event);
        }
    });

    // Read with detailed logging
    info!("Starting blocking read task...");
    let source_clone = Arc::clone(&source);
    let result = tokio::task::spawn_blocking(move || {
        info!("Inside blocking task, creating SyncReader");
        let mut reader = SyncReader::new(source_clone, SyncReaderParams::default());
        let mut all_data = Vec::new();
        let mut buf = [0u8; 100];
        let mut read_count = 0;
        let mut total_bytes = 0;

        info!("Starting read loop...");
        loop {
            let n = reader.read(&mut buf).unwrap();
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

    // Calculate expected size
    let expected = [
        test_segment_data(1, 0),
        test_segment_data(1, 1),
        test_segment_data(1, 2),
    ]
    .concat();

    info!(
        "Expected {} bytes, got {} bytes",
        expected.len(),
        result.len()
    );

    // Compare sizes first
    if result.len() != expected.len() {
        panic!(
            "Size mismatch: expected {} bytes, got {} bytes",
            expected.len(),
            result.len()
        );
    }

    // Check first 100 bytes for debugging
    info!(
        "First 100 bytes match: {}",
        result[..100] == expected[..100]
    );

    // Full comparison
    assert_eq!(result, expected, "Data content mismatch");

    info!("Test passed!");

    // Cancel to shutdown cleanly
    drop(events_handle);
}
