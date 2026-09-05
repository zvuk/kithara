#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    host::HostConfig,
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, timeout},
        tokio,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, kithara,
    offline::{OfflineQueue, drive_queue_ticks},
    temp_dir,
};

use crate::bufpool_ext::{TestPools, pools};

async fn wait_for_failed(
    rx: &mut EventReceiver,
    queue: &QueueControl<TestPools>,
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
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(1)
                .base_delay(Duration::from_millis(1))
                .max_delay(Duration::from_millis(10))
                .build(),
        )
        .build();
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );

    let session = HostConfig::offline(pools.clone())
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        session,
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .expect("create product offline queue");
    let tick_handle = tokio::task::spawn(drive_queue_ticks(
        queue.control(),
        Duration::from_millis(50),
    ));

    let cfg = ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid URL"))
        .downloader(downloader)
        .store(
            AssetStore::builder(pools)
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .build();

    let mut rx = queue.subscribe();
    let id = queue
        .append(TrackSource::Config(Box::new(cfg)))
        .expect("append stalled playlist track");
    let _ = queue.select(id, Transition::None);

    let err = wait_for_failed(&mut rx, &queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!err.is_empty(), "Failed status must carry a typed error");

    tick_handle.abort();
}
