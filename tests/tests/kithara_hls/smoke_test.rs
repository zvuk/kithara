#![forbid(unsafe_code)]

use std::error::Error as StdError;

use kithara::{
    assets::StoreOptions,
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::hls_fixture::{HlsStreamBuilder, TestServer};
use kithara_platform::{
    time::{Duration, sleep, timeout},
    tokio::{sync::oneshot, task::spawn},
};
use kithara_test_utils::{TestTempDir, temp_dir, tracing_setup};
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

// Test Cases

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)))]
async fn test_hls_session_creation(
    _tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session creation with URL: {}", test_stream_url);

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    // Create HLS config with events
    let config = HlsConfig::new(test_stream_url.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(bus);

    // Test: Open HLS source
    let _stream = Stream::<Hls>::new(config).await?;
    info!("HLS source opened successfully");

    // Spawn a task to consume events (prevent channel from filling up)
    let (events_sender, events_receiver) = oneshot::channel::<u64>();

    spawn(async move {
        let mut event_count = 0;
        while let Ok(Ok(event)) = timeout(Duration::from_millis(100), events_rx.recv()).await {
            event_count += 1;
            if event_count <= 3 {
                info!("Event {}: {:?}", event_count, event);
            }
        }
        let _ = events_sender.send(event_count);
    });

    // Get event count with timeout
    let event_count = events_receiver.await?;
    info!("Total events received: {}", event_count);

    // If we got here without errors, the test passes
    Ok(())
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)))]
#[case::plain(false)]
#[case::with_init(true)]
async fn test_hls_stream_creation(
    _tracing_setup: (),
    temp_dir: TestTempDir,
    #[case] with_init: bool,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;

    let builder = HlsStreamBuilder::new();
    let builder = if with_init {
        builder.with_init()
    } else {
        builder
    };

    let _stream = builder
        .build(&server, temp_dir.path(), CancellationToken::new())
        .await;

    info!("HLS stream created successfully (init={})", with_init);
    Ok(())
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)))]
async fn test_hls_session_events_consumption(
    _tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8")?;
    info!("Testing HLS session events consumption");

    // Create event bus
    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::new(test_stream_url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(bus);

    let _stream = Stream::<Hls>::new(config).await?;

    // Try to receive events with timeout
    let wait = Duration::from_millis(500);
    let event_result = timeout(wait, events_rx.recv()).await;

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

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)))]
async fn test_hls_invalid_url_handling(
    _tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    // Test with invalid URL
    let invalid_url = "http://invalid-domain-that-does-not-exist-12345.com/master.m3u8";
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        // If URL parses, try to open HLS (should fail with network error)
        let config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp_dir.path()))
            .with_cancel(CancellationToken::new());

        let result = Stream::<Hls>::new(config).await;
        assert!(result.is_err(), "invalid URL should fail, got Ok");
    } else {
        // Invalid URL string - parse should fail
        assert!(url_result.is_err());
    }

    Ok(())
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)))]
async fn test_hls_session_drop_cleanup(
    _tracing_setup: (),
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS session drop cleanup");

    // Create and immediately drop session
    let stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), CancellationToken::new())
        .await;
    drop(stream);

    // Wait a bit to ensure cleanup happens
    sleep(Duration::from_millis(100)).await;

    info!("HLS session dropped without issues");
    Ok(())
}
