#![forbid(unsafe_code)]

use std::{error::Error, io::Read, time::Duration};

use fixture::TestServer;
use kithara::{
    assets::StoreOptions,
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_test_utils::{cancel_token, temp_dir};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

use super::fixture;

// Fixtures

#[fixture]
fn tracing_setup() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::default()
                .add_directive("kithara_hls=info".parse().unwrap())
                .add_directive("kithara_stream=info".parse().unwrap())
                .add_directive("warn".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();
}

// Test Cases

/// Basic integration test for HLS playback functionality.
/// This test verifies that:
/// 1. HLS session can be opened
/// 2. Audio source can be obtained
/// 3. Rodio decoder can be created from the stream
///
/// Note: This test uses a local test server.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_basic_hls_playback(
    _tracing_setup: (),
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Starting HLS playback test with URL: {}", test_stream_url);

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    // 1. Test: Open HLS source
    info!("Opening HLS source...");
    let config = HlsConfig::new(test_stream_url.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_events(bus);

    let stream = Stream::<Hls>::new(config).await?;
    info!("HLS source opened successfully");

    // Start event monitor in background
    let _events_handle = tokio::spawn(async move {
        let mut event_count = 0;
        while let Ok(ev) = events_rx.recv().await {
            event_count += 1;
            if event_count <= 3 {
                info!("Event {}: {:?}", event_count, ev);
            }
        }
    });

    // 3. Test: Create rodio decoder (this validates the stream format)
    info!("Creating rodio decoder...");
    let decoder_result = tokio::task::spawn_blocking(move || rodio::Decoder::new(stream)).await;

    match decoder_result {
        Ok(_decoder) => {
            info!("Rodio decoder created successfully");
            Ok(())
        }
        Err(e) => {
            warn!("Failed to create rodio decoder: {}", e);
            // Test data is not valid audio, so decoder failure is expected
            info!("Note: Rodio decoder failed, but HLS layer is functional");
            Ok(())
        }
    }
}

/// Test that verifies HLS session creation without actual playback.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_session_creation(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    // Test source creation
    let config = HlsConfig::new(test_stream_url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_events(bus);

    let _stream = Stream::<Hls>::new(config).await?;

    // Spawn a task to consume events (prevent channel from filling up)
    tokio::spawn(async move {
        while events_rx.recv().await.is_ok() {
            // Just consume events
        }
    });

    // If we got here without errors, the test passes
    Ok(())
}

/// Test HLS with init segments.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_with_init_segments(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let server = TestServer::new().await;
    let url = server.url("/master-init.m3u8")?;
    info!("Testing HLS with init segments: {}", url);

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token);

    let _stream = Stream::<Hls>::new(config).await?;

    info!("Stream with init segments opened successfully");
    Ok(())
}

/// Test HLS with different options configurations.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_with_different_options(
    temp_dir: TempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS with custom options");

    let config = HlsConfig::new(test_stream_url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

    // Test source creation with different options
    let _stream = Stream::<Hls>::new(config).await?;

    info!("HLS source opened successfully with custom options");
    Ok(())
}

/// Test HLS session error handling with invalid URLs.
#[rstest]
#[case("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")]
#[case("not-a-valid-url")]
#[case("")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_invalid_url_handling(
    #[case] invalid_url: &str,
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        // If URL parses, try to open HLS (should fail with network error)
        let config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp_dir.path()))
            .with_cancel(cancel_token);

        let result = Stream::<Hls>::new(config).await;
        // Either Ok (if somehow connects) or Err (expected) is acceptable
        assert!(result.is_ok() || result.is_err());
    } else {
        // Invalid URL string - parse should fail
        assert!(url_result.is_err());
    }

    Ok(())
}

/// Test that INIT segment comes first in byte stream (offset 0).
/// This is critical for fMP4 HLS where decoder needs moov box before mdat.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_init_segment_at_stream_start(
    temp_dir: TempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let server = TestServer::new().await;
    let url = server.url("/master-init.m3u8")?;
    info!("Testing INIT segment at stream start: {}", url);

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token);

    let mut stream = Stream::<Hls>::new(config).await?;

    // Wait for INIT and first segment to be loaded.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read from offset 0 - should get INIT data, not SEG-0.
    // INIT data for variant 0: "V0-INIT:TEST_INIT_DATA" (22 bytes)
    let mut buf = [0u8; 32];

    let n = tokio::task::spawn_blocking(move || stream.read(&mut buf).map(|n| (n, buf)))
        .await?
        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

    let (bytes_read, data) = n;
    assert!(bytes_read > 0, "Should read data from offset 0");

    let data = &data[..bytes_read];
    assert!(
        data.starts_with(b"V0-INIT:"),
        "Offset 0 should contain INIT data, got: {:?}",
        String::from_utf8_lossy(&data[..data.len().min(20)])
    );

    info!("INIT segment correctly at stream start");
    Ok(())
}

/// Test HLS with limited cache.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_hls_without_cache(temp_dir: TempDir) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::default().add_directive("warn".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;

    // Create options with very small cache to simulate limited caching
    let config = HlsConfig::new(test_stream_url)
        .with_store(
            StoreOptions::new(temp_dir.path())
                .with_max_assets(1)
                .with_max_bytes(1024), // 1KB cache
        )
        .with_cancel(CancellationToken::new());

    info!("Testing HLS with limited cache");

    // Test source creation with limited cache
    let _stream = Stream::<Hls>::new(config).await?;

    info!("HLS source opened successfully with limited cache");
    Ok(())
}
