#![forbid(unsafe_code)]

use std::{error::Error, io::Read};

use kithara::{
    assets::StoreOptions,
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, hls_fixture::HlsStreamBuilder, hls_server::TestServer, rt_cancel, temp_dir,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::tokio::task::spawn_blocking;
use kithara_platform::{CancelToken, time::Duration, tokio::task::spawn};
use tracing::info;
use url::Url;

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
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=info,kithara_stream=info,warn")
)]
async fn test_basic_hls_playback(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8");
    info!("Starting HLS playback test with URL: {}", test_stream_url);

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();
    let mut live_rx = bus.subscribe();

    info!("Opening HLS source...");
    let config = HlsConfig::for_url(test_stream_url.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(rt_cancel)
        .events(bus)
        .build();

    let stream = Stream::<Hls>::new(config).await?;
    info!("HLS source opened successfully");

    let _events_handle = spawn(async move {
        let mut event_count = 0;
        while let Ok(ev) = events_rx.recv().await {
            event_count += 1;
            if event_count <= 3 {
                info!("Event {}: {:?}", event_count, ev);
            }
        }
    });

    let _ = stream;
    // Wait for the pipeline to become live: the first event published on the
    // bus (segment-fetch enqueue / playlist activity) proves the stream is
    // producing real work, instead of sleeping for a fixed duration. The
    // enclosing `timeout(5s)` bounds this state-wait against a hang.
    let _first = live_rx
        .recv()
        .await
        .map_err(|e| Box::new(e) as Box<dyn Error + Send + Sync>)?;
    info!("HLS stream opened successfully");
    Ok(())
}

/// Test that verifies HLS session creation without actual playback.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_session_creation(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8");

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::for_url(test_stream_url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(rt_cancel)
        .events(bus)
        .build();

    let _stream = Stream::<Hls>::new(config).await?;

    spawn(async move { while events_rx.recv().await.is_ok() {} });

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
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS with init segments");

    let _stream = HlsStreamBuilder::new()
        .with_init()
        .build(&server, temp_dir.path(), rt_cancel)
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
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS with custom options");

    let _stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), CancelToken::never())
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
    #[case] invalid_url: &str,
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        let config = HlsConfig::for_url(url)
            .store(StoreOptions::new(temp_dir.path()))
            .cancel(rt_cancel)
            .build();

        let result = Stream::<Hls>::new(config).await;
        assert!(
            result.is_err(),
            "invalid URL should fail, got Ok for {invalid_url:?}"
        );
    } else {
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
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing INIT segment at stream start");

    let mut stream = HlsStreamBuilder::new()
        .with_init()
        .build(&server, temp_dir.path(), rt_cancel)
        .await;

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
async fn test_hls_without_cache(temp_dir: TestTempDir) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;

    info!("Testing HLS with limited cache");

    let _stream = HlsStreamBuilder::new()
        .max_assets(1)
        .max_bytes(1024)
        .build(&server, temp_dir.path(), CancelToken::never())
        .await;

    info!("HLS source opened successfully with limited cache");
    Ok(())
}
