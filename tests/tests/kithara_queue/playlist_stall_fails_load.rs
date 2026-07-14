#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::StoreOptions,
    events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, timeout},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession, temp_dir,
};

async fn wait_for_failed(
    rx: &mut EventReceiver,
    queue: &Queue,
    id: TrackId,
    deadline: Duration,
) -> Result<String, String> {
    if let Some(entry) = queue.track(id)
        && let TrackStatus::Failed(err) = &entry.status
    {
        return Ok(err.clone());
    }
    let start = Instant::now();
    while start.elapsed() < deadline {
        match timeout(Duration::from_millis(500), rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status })))
                if tid == id =>
            {
                match status {
                    TrackStatus::Failed(err) => return Ok(err),
                    TrackStatus::Loaded => {
                        return Err("track loaded from a stalled playlist".into());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Err(format!(
        "track neither failed nor loaded within {deadline:?} — load hung on the stalled playlist"
    ))
}

/// A master-playlist GET whose body stalls after the first bytes (headers
/// sent, connection open, no further data — the throttling-CDN shape) must
/// fail the track load with a typed error within the net budget
/// (`inactivity_timeout` × retries), never park it in `Loading` forever.
/// Pins the production hang where `Resource::new` blocked indefinitely on
/// an unbounded playlist body read.
#[kithara::test(tokio, timeout(Duration::from_secs(60)))]
async fn stalled_master_playlist_fails_load(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let playlist = b"#EXTM3U\n#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=66005\nindex.m3u8\n";
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(playlist.to_vec()),
            content_type: Some("application/vnd.apple.mpegurl"),
        },
        delivery: Delivery::StallAfter { after_bytes: 8 },
    });
    let url = handle.child_url("master.m3u8");

    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(200))
        .retry_policy(RetryPolicy::new(
            1,
            Duration::from_millis(1),
            Duration::from_millis(10),
        ))
        .build();
    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(net, CancelToken::never()))
            .build(),
    );

    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            kithara::platform::time::sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("valid URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader)
        .store(StoreOptions::new(temp_dir.path()))
        .build();

    let mut rx = queue.subscribe();
    let id = queue.append(TrackSource::Config(Box::new(cfg)));
    let _ = queue.select(id, Transition::None);

    let err = wait_for_failed(&mut rx, &queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!err.is_empty(), "Failed status must carry a typed error");

    tick_handle.abort();
}
