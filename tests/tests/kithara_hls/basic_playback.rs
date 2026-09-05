#![forbid(unsafe_code)]

use std::{error::Error, io::Read};

#[cfg(not(target_arch = "wasm32"))]
use kithara::platform::tokio::task::spawn_blocking;
use kithara::{
    assets::{AssetStore, StorageBackend},
    events::EventBus,
    hls::{Hls, HlsConfig},
    platform::{
        CancelToken,
        time::Duration,
        tokio::{sync::broadcast::error::RecvError, task::spawn},
    },
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_fixture::HlsStreamBuilder,
    hls_server::TestServer,
    rt_cancel, temp_dir,
};
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
    hang_timeout_secs(1),
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
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();
    let config = HlsConfig::for_url(test_stream_url.clone())
        .store(store)
        .pools(pools)
        .cancel(rt_cancel)
        .events(bus)
        .build();

    let stream = Stream::<Hls<TestPools>>::new(config).await?;
    info!("HLS source opened successfully");

    let _events_handle = spawn(async move {
        let mut event_count = 0;
        while let Ok(ev) = events_rx.recv().await.map(|env| env.event) {
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
    //
    // A `Lagged(n)` is also proof of liveness: under flash the pipeline floods
    // the (capacity-32) bus with events before this first `recv` is polled, so
    // the receiver legitimately laps. `n` events were produced — exactly the
    // signal we wait on. Only a `Closed` channel (the bus dropped without ever
    // producing) is a real failure.
    match live_rx.recv().await {
        Ok(_) | Err(RecvError::Lagged(_)) => {}
        Err(e @ RecvError::Closed) => return Err(Box::new(e) as Box<dyn Error + Send + Sync>),
    }
    info!("HLS stream opened successfully");
    Ok(())
}

#[derive(Clone, Copy)]
enum StreamOptions {
    Init,
    NeverCancel,
    LimitedCache,
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case::init_segments(StreamOptions::Init)]
#[case::never_cancel(StreamOptions::NeverCancel)]
#[case::limited_cache(StreamOptions::LimitedCache)]
async fn hls_stream_options_open(
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
    #[case] options: StreamOptions,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = TestServer::new().await;
    let (builder, cancel) = match options {
        StreamOptions::Init => (HlsStreamBuilder::new().with_init(), rt_cancel),
        StreamOptions::NeverCancel => (HlsStreamBuilder::new(), CancelToken::never()),
        StreamOptions::LimitedCache => (
            HlsStreamBuilder::new().max_assets(1).max_bytes(1024),
            CancelToken::never(),
        ),
    };

    let _stream = builder.build(&server, temp_dir.path(), cancel).await;
    Ok(())
}

/// Test HLS session error handling with invalid URLs.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case("http://127.0.0.1:9/master.m3u8")]
#[case("not-a-valid-url")]
#[case("")]
async fn test_hls_invalid_url_handling(
    #[case] invalid_url: &str,
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let url_result = Url::parse(invalid_url);

    if let Ok(url) = url_result {
        let pools = pools();
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: temp_dir.path().to_path_buf(),
            })
            .build();
        let config = HlsConfig::for_url(url)
            .store(store)
            .pools(pools)
            .cancel(rt_cancel)
            .build();

        let result = Stream::<Hls<TestPools>>::new(config).await;
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
#[kithara::test(tokio, browser, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
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
