#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{AudioEvent, Event, TrackId},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir,
    bufpool_ext::{Pools, TestPools, pools},
    kithara,
    offline::{OfflineQueue, drive_queue_ticks},
    temp_dir,
    test_defaults::Consts as Shared,
    waits::{wait_for_event, wait_for_loader_done_event, wait_for_position_event},
};
use kithara_test_fixtures::SignalAsset;

const SAVE_AFTER_SECS: f64 = 4.0;

fn new_queue(pools: &Pools, store: AssetStore<TestPools>) -> OfflineQueue<TestPools> {
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    OfflineQueue::new(
        HostConfig::offline(pools.clone())
            .pacing(Duration::from_millis(10))
            .build(),
        Queue::new(QueueConfig::builder().player(player).store(store).build()),
    )
    .expect("create product offline queue")
}

fn new_downloader(pools: Pools) -> Downloader {
    Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools,
            CancelToken::never(),
        ))
        .build(),
    )
}

fn append_track(
    queue: &QueueControl<TestPools>,
    url: &str,
    downloader: &Downloader,
    store: &AssetStore<TestPools>,
) -> TrackId {
    let cfg = ResourceConfig::for_src(ResourceSrc::parse(url).expect("valid URL"))
        .downloader(downloader.clone())
        .store(store.clone())
        .build();
    queue
        .append(TrackSource::Config(Box::new(cfg)))
        .expect("append resume track")
}

#[kithara::test(tokio, timeout(Duration::from_secs(90)))]
async fn playback_starts_from_the_seeked_position(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let url = helper.signal(SignalAsset::MP3_SINE880_30S);

    let first_pools = pools();
    let first_store = AssetStore::builder(first_pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let first_queue = new_queue(&first_pools, first_store.clone());
    let first_downloader = new_downloader(first_pools);
    let first_tick = tokio::task::spawn(drive_queue_ticks(
        first_queue.control(),
        Duration::from_millis(50),
    ));
    let mut first_rx = first_queue.subscribe();
    let first_id = append_track(&first_queue, url.as_str(), &first_downloader, &first_store);
    first_queue
        .select(first_id, Transition::None)
        .expect("select first session track");
    wait_for_loader_done_event(
        &mut first_rx,
        &first_queue,
        first_id,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    // Without this the first session never advances and no position worth
    // saving is ever reached.
    first_queue.play();

    let played = wait_for_position_event(
        &mut first_rx,
        &first_queue,
        SAVE_AFTER_SECS,
        Duration::from_secs(15),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));
    assert!(
        played >= SAVE_AFTER_SECS,
        "precondition: playback reached only {played:.2}s"
    );
    first_queue.pause();
    let saved = played;
    first_queue.clear();
    first_tick.abort();
    let _ = first_tick.await;
    drop(first_queue);
    drop(first_downloader);

    let second_pools = pools();
    let second_store = AssetStore::builder(second_pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let second_queue = new_queue(&second_pools, second_store.clone());
    let second_downloader = new_downloader(second_pools);
    let second_tick = tokio::task::spawn(drive_queue_ticks(
        second_queue.control(),
        Duration::from_millis(50),
    ));
    let mut second_rx = second_queue.subscribe();
    let second_id = append_track(
        &second_queue,
        url.as_str(),
        &second_downloader,
        &second_store,
    );
    second_queue
        .select(second_id, Transition::None)
        .expect("select second session track");
    wait_for_loader_done_event(
        &mut second_rx,
        &second_queue,
        second_id,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));
    // The app restores the saved position on a freshly loaded, not-yet-playing
    // track and only then starts playback.
    let outcome = second_queue.seek(saved);
    assert!(
        outcome.is_ok(),
        "precondition: seek to saved position {saved:.2}s failed: {:?}",
        outcome.err()
    );
    // `SeekComplete` is published by the decoder once it reads at the new
    // position, so it cannot arrive while the engine is still paused.
    second_queue.play();
    wait_for_event(
        &mut second_rx,
        "saved-position seek completion",
        |event| {
            matches!(
                event,
                Event::Audio(AudioEvent::SeekComplete { position, .. })
                    if (position.as_secs_f64() - saved).abs() < 1.0
            )
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    let mut resumed_at = None;
    wait_for_event(
        &mut second_rx,
        "first playback progress after saved-position seek",
        |event| {
            let Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) = event else {
                return false;
            };
            resumed_at = Some(*position_ms as f64 / 1000.0);
            true
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap_or_else(|error| panic!("playback did not resume from the saved position: {error}"));
    let resumed_at = resumed_at.expect("matched playback progress carries a position");
    assert!(
        resumed_at >= saved - 1.0,
        "playback fell back to {resumed_at:.2}s after seeking to \
         saved position {saved:.2}s"
    );

    second_tick.abort();
    let _ = second_tick.await;
}
