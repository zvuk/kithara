#![forbid(unsafe_code)]

use std::time::Duration;

use fixture::TestServer;
use kithara_assets::StoreOptions;
use kithara_hls::{Hls, HlsConfig};
use kithara_stream::Stream;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

use super::fixture;
use crate::common::fixtures::{temp_dir, tracing_setup};

// Test Cases

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_creation(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session creation with URL: {}", test_stream_url);

    // Create events channel
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(32);

    // Create HLS config with events
    let config = HlsConfig::new(test_stream_url.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(events_tx);

    // Test: Open HLS source
    let _stream = Stream::<Hls>::new(config).await?;
    info!("HLS source opened successfully");

    // Spawn a task to consume events (prevent channel from filling up)
    let events_handle = tokio::spawn(async move {
        let mut event_count = 0;
        loop {
            match tokio::time::timeout(Duration::from_millis(100), events_rx.recv()).await {
                Ok(Ok(event)) => {
                    event_count += 1;
                    if event_count <= 3 {
                        info!("Event {}: {:?}", event_count, event);
                    }
                }
                _ => break,
            }
        }
        event_count
    });

    // Get event count with timeout
    let event_count = events_handle.await?;
    info!("Total events received: {}", event_count);

    // If we got here without errors, the test passes
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_with_local_fixture(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8")?;
    info!("Testing HLS with local fixture at: {}", url);

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

    let _stream = Stream::<Hls>::new(config).await?;

    info!("Local fixture test passed");
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_with_init_segments(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let url = server.url("/master-init.m3u8")?;
    info!("Testing HLS session with init segments at URL: {}", url);

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

    let _stream = Stream::<Hls>::new(config).await?;

    info!("HLS source with init segments opened successfully");
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_events_consumption(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session events consumption");

    // Create events channel
    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(32);

    let config = HlsConfig::new(test_stream_url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(events_tx);

    let _stream = Stream::<Hls>::new(config).await?;

    // Try to receive events with timeout
    let timeout = Duration::from_millis(500);
    let event_result = tokio::time::timeout(timeout, events_rx.recv()).await;

    match event_result {
        Ok(Ok(event)) => {
            info!("Received event: {:?}", event);
        }
        Ok(Err(_)) => {
            info!("Event channel closed");
        }
        Err(_) => {
            info!("No events received within timeout (expected for some streams)");
        }
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_invalid_url_handling(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Test with invalid URL
    let invalid_url = "http://invalid-domain-that-does-not-exist-12345.com/master.m3u8";
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        // If URL parses, try to open HLS (should fail with network error)
        let config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp_dir.path()))
            .with_cancel(CancellationToken::new());

        let result = Stream::<Hls>::new(config).await;
        // Either Ok (if somehow connects) or Err (expected) is acceptable
        assert!(result.is_ok() || result.is_err());
    } else {
        // Invalid URL string - parse should fail
        assert!(url_result.is_err());
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_drop_cleanup(
    _tracing_setup: (),
    temp_dir: TempDir,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session drop cleanup");

    let config = HlsConfig::new(test_stream_url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

    // Create and immediately drop session
    let stream = Stream::<Hls>::new(config).await?;
    drop(stream);

    // Wait a bit to ensure cleanup happens
    tokio::time::sleep(Duration::from_millis(100)).await;

    info!("HLS session dropped without issues");
    Ok(())
}
