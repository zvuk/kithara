#![forbid(unsafe_code)]

use std::error::Error as StdError;

use kithara::{
    assets::StoreOptions,
    events::EventBus,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir, hls_fixture::HlsStreamBuilder, hls_server::TestServer, temp_dir,
};
use kithara_platform::{
    time::{Duration, sleep, timeout},
    tokio::{sync::oneshot, task::spawn},
};
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_session_creation(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8");
    info!("Testing HLS session creation with URL: {}", test_stream_url);

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::for_url(test_stream_url.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancellationToken::new())
        .events(bus)
        .build();

    let _stream = Stream::<Hls>::new(config).await?;
    info!("HLS source opened successfully");

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

    let event_count = events_receiver.await?;
    info!("Total events received: {}", event_count);

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::plain(false)]
#[case::with_init(true)]
async fn test_hls_stream_creation(
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

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_session_events_consumption(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    let test_stream_url = server.url("/master.m3u8");
    info!("Testing HLS session events consumption");

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();

    let config = HlsConfig::for_url(test_stream_url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancellationToken::new())
        .events(bus)
        .build();

    let _stream = Stream::<Hls>::new(config).await?;

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

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_invalid_url_handling(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let invalid_url = "http://invalid-domain-that-does-not-exist-12345.com/master.m3u8";
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        let config = HlsConfig::for_url(url)
            .store(StoreOptions::new(temp_dir.path()))
            .cancel(CancellationToken::new())
            .build();

        let result = Stream::<Hls>::new(config).await;
        assert!(result.is_err(), "invalid URL should fail, got Ok");
    } else {
        assert!(url_result.is_err());
    }

    Ok(())
}

#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_hls_session_drop_cleanup(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    let server = TestServer::new().await;
    info!("Testing HLS session drop cleanup");

    let stream = HlsStreamBuilder::new()
        .build(&server, temp_dir.path(), CancellationToken::new())
        .await;
    drop(stream);

    sleep(Duration::from_millis(100)).await;

    info!("HLS session dropped without issues");
    Ok(())
}
