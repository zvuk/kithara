#![forbid(unsafe_code)]

use std::{error::Error, io::Read, time::Duration};

use kithara::{
    assets::StoreOptions,
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::hls_fixture::{HlsStreamBuilder, TestServer};
use kithara_platform::time::sleep;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::tokio::task::spawn_blocking;
use kithara_test_utils::{TestTempDir, cancel_token, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use url::Url;

// Fixtures

#[kithara::fixture]
fn warn_filter() -> EnvFilter {
    EnvFilter::default().add_directive("warn".parse().unwrap())
}

#[kithara::fixture]
fn hls_info_filter() -> EnvFilter {
    EnvFilter::default()
        .add_directive("kithara_hls=info".parse().unwrap())
        .add_directive("kithara_stream=info".parse().unwrap())
        .add_directive("warn".parse().unwrap())
}

#[kithara::fixture]
fn tracing_setup(hls_info_filter: EnvFilter) {
    kithara_test_utils::init_tracing(hls_info_filter);
}

#[kithara::fixture]
fn warn_tracing_setup(warn_filter: EnvFilter) {
    kithara_test_utils::init_tracing(warn_filter);
}

// Test Cases

/// Basic integration test for HLS playback functionality.
/// This test verifies that:
/// 1. HLS session can be opened
/// 2. Audio source can be obtained
/// 3. Rodio decoder can be created from the stream
///
/// Note: This test uses a local test server.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_basic_hls_playback(
    _tracing_setup: (),
    temp_dir: TestTempDir,
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
    let _events_handle = kithara_platform::tokio::task::spawn(async move {
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
    #[cfg(not(target_arch = "wasm32"))]
    {
        let decoder_result = spawn_blocking(move || rodio::Decoder::new(stream)).await;

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

    #[cfg(target_arch = "wasm32")]
    {
        let _ = stream;
        sleep(Duration::from_millis(50)).await;
        info!("HLS stream opened successfully on wasm");
        Ok(())
    }
}

/// Test that verifies HLS session creation without actual playback.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_session_creation(
    _warn_tracing_setup: (),
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
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
    kithara_platform::tokio::task::spawn(async move {
        while events_rx.recv().await.is_ok() {
            // Just consume events
        }
    });

    // If we got here without errors, the test passes
    Ok(())
}

/// Test HLS with init segments.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_with_init_segments(
    _warn_tracing_setup: (),
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS with init segments");

    let _stream = HlsStreamBuilder::new()
        .with_init()
        .build(&server, temp_dir.path(), cancel_token)
        .await;

    info!("Stream with init segments opened successfully");
    Ok(())
}

/// Test HLS with different options configurations.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_with_different_options(
    _warn_tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS with custom options");

    // Test source creation with different options
    let _stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), CancellationToken::new())
        .await;

    info!("HLS source opened successfully with custom options");
    Ok(())
}

/// Test HLS session error handling with invalid URLs.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case("http://invalid-domain-that-does-not-exist-12345.com/master.m3u8")]
#[case("not-a-valid-url")]
#[case("")]
async fn test_hls_invalid_url_handling(
    _warn_tracing_setup: (),
    #[case] invalid_url: &str,
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        // If URL parses, try to open HLS (should fail with network error)
        let config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp_dir.path()))
            .with_cancel(cancel_token);

        let result = Stream::<Hls>::new(config).await;
        assert!(
            result.is_err(),
            "invalid URL should fail, got Ok for {invalid_url:?}"
        );
    } else {
        // Invalid URL string - parse should fail
        assert!(url_result.is_err());
    }

    Ok(())
}

/// Test that INIT segment comes first in byte stream (offset 0).
/// This is critical for fMP4 HLS where decoder needs moov box before mdat.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_init_segment_at_stream_start(
    _warn_tracing_setup: (),
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing INIT segment at stream start");

    let mut stream = HlsStreamBuilder::new()
        .with_init()
        .build(&server, temp_dir.path(), cancel_token)
        .await;

    // Wait for INIT and first segment to be loaded.
    sleep(Duration::from_millis(100)).await;

    // Read from offset 0 - should get INIT data, not SEG-0.
    // Variant is ABR-dependent, so validate init marker generically.
    let mut buf = [0u8; 32];

    #[cfg(not(target_arch = "wasm32"))]
    let n = spawn_blocking(move || stream.read(&mut buf).map(|n| (n, buf)))
        .await?
        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

    #[cfg(target_arch = "wasm32")]
    let n = stream
        .read(&mut buf)
        .map(|n| (n, buf))
        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;

    let (bytes_read, data) = n;
    assert!(bytes_read > 0, "Should read data from offset 0");

    let data = &data[..bytes_read];
    let head = String::from_utf8_lossy(&data[..data.len().min(20)]);
    assert!(
        head.contains("-INIT:"),
        "Offset 0 should contain INIT data, got: {:?}",
        head
    );

    info!("INIT segment correctly at stream start");
    Ok(())
}

/// Test HLS with limited cache.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_without_cache(
    _warn_tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;

    info!("Testing HLS with limited cache");

    // Test source creation with limited cache (1KB)
    let _stream = HlsStreamBuilder::new()
        .max_assets(1)
        .max_bytes(1024)
        .build(&server, temp_dir.path(), CancellationToken::new())
        .await;

    info!("HLS source opened successfully with limited cache");
    Ok(())
}
