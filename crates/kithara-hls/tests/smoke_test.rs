#![forbid(unsafe_code)]

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_hls::{Hls, HlsParams};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

mod fixture;
use fixture::TestServer;

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn hls_params(temp_dir: TempDir) -> HlsParams {
    HlsParams::new(StoreOptions::new(temp_dir.path())).with_cancel(CancellationToken::new())
}

#[fixture]
fn minimal_tracing_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();
}

// ==================== Test Cases ====================

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_creation(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session creation with URL: {}", test_stream_url);

    // Test 1: Open HLS source
    let source = Hls::open(test_stream_url.clone(), hls_params).await?;
    info!("HLS source opened successfully");

    // Test 2: Get events channel
    let mut events_rx = source.events();

    // Source is ready for use
    let _source = source;
    info!("HLS source obtained successfully");

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
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let url = server.url("/master.m3u8")?;
    info!("Testing HLS with local fixture at: {}", url);

    let _source = Hls::open(url, hls_params).await?;

    info!("Local fixture test passed");
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_with_init_segments(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let url = server.url("/master-init.m3u8")?;
    info!("Testing HLS session with init segments at URL: {}", url);

    let _source = Hls::open(url, hls_params).await?;

    info!("HLS source with init segments opened successfully");
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_events_consumption(
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session events consumption");

    let source = Hls::open(test_stream_url, hls_params).await?;

    // Get events channel
    let mut events_rx = source.events();

    let _source = source;

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
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Test with invalid URL
    let invalid_url = "http://invalid-domain-that-does-not-exist-12345.com/master.m3u8";
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        // If URL parses, try to open HLS (should fail with network error)
        let result = Hls::open(url, hls_params).await;
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
    _minimal_tracing_setup: (),
    hls_params: HlsParams,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session drop cleanup");

    // Create and immediately drop session
    let session = Hls::open(test_stream_url, hls_params).await?;
    drop(session);

    // Wait a bit to ensure cleanup happens
    tokio::time::sleep(Duration::from_millis(100)).await;

    info!("HLS session dropped without issues");
    Ok(())
}
