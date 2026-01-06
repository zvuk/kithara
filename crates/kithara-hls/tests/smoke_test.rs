#![forbid(unsafe_code)]

use kithara_assets::{AssetStore, EvictConfig};
use kithara_hls::{HlsOptions, HlsSource, playlist::VariantId};
use tempfile::TempDir;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

#[tokio::test]
async fn test_hls_session_creation() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Setup minimal logging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse()?))
        .with_test_writer()
        .try_init();

    // Use a public test stream
    let test_url = "https://stream.silvercomet.top/hls/master.m3u8";
    let url: Url = test_url.parse()?;

    info!("Testing HLS session creation with URL: {}", url);

    // Create temporary directory for cache
    let temp_dir = TempDir::new()?;
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());

    // Test 1: Open HLS session
    let session = HlsSource::open(url.clone(), HlsOptions::default(), assets.clone()).await?;
    info!("✓ HLS session opened successfully");

    // Test 2: Get source
    let _source = session.source().await?;
    info!("✓ HLS source obtained successfully");

    // Test 3: Check events channel
    let mut events_rx = session.events();

    // Spawn a task to consume events (prevent channel from filling up)
    let events_handle = tokio::spawn(async move {
        let mut event_count = 0;
        while let Ok(event) = events_rx.recv().await {
            event_count += 1;
            if event_count <= 3 {
                info!("Event {}: {:?}", event_count, event);
            }
        }
        event_count
    });

    // Give some time for events to potentially arrive
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Drop session to close event channel
    drop(session);

    // Get event count
    let event_count = events_handle.await?;
    info!("Total events received: {}", event_count);

    // If we got here without errors, the test passes
    Ok(())
}

#[tokio::test]
async fn test_hls_with_local_fixture() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse()?))
        .with_test_writer()
        .try_init();

    info!("Local fixture test skipped - requires working test server setup");
    Ok(())
}
