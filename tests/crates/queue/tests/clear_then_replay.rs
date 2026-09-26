#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    host::HostConfig,
    platform::time::Duration,
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, TrackSource, Transition},
};
use kithara_integration_tests::{
    TestServerHelper,
    hls_fixture::create_test_downloader,
    kithara,
    offline::{OfflineQueue, QueueTicker, RENDER_PACE},
    served_mp3, temp_dir,
    waits::wait_for_position_event,
};
use url::Url;

use crate::bufpool_ext::pools;

/// Emptying the queue while it plays must not cost the next track its output.
///
/// `clear` stops the engine and disarms every selection, so what comes after it
/// starts from the same state a fresh player would — except that the output is
/// already built. A player that only arms its output on the first start plays
/// the replacement silently: the track loads, its duration lands, and the
/// playhead never leaves zero.
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn a_cleared_queue_plays_the_track_appended_after_it(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    let (_helper, url) = served_mp3;

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let session_pools = pools();
    let session = HostConfig::offline(session_pools.clone()).build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(session_pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::paced(
        session,
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .expect("create product offline queue");
    let mut tick_handle = QueueTicker::spawn(queue.control(), Duration::from_millis(50));

    let track_config = || {
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
            .downloader(create_test_downloader())
            .store(store.clone())
            .build()
    };

    let mut rx = queue.subscribe();
    let first = track_config();
    queue
        .run(move |q| q.append(TrackSource::Config(Box::new(first))))
        .await
        .expect("append the first track");
    queue.run(kithara::queue::QueueControl::play).await;
    wait_for_position_event(&mut rx, &queue, 0.2, Duration::from_secs(60))
        .await
        .expect("the first track must play before the queue is emptied");

    queue.run(kithara::queue::QueueControl::clear).await;
    assert_eq!(queue.control().len(), 0, "clear must empty the queue");

    let mut rx = queue.subscribe();
    let second = track_config();
    let replacement = queue
        .run(move |q| q.append(TrackSource::Config(Box::new(second))))
        .await
        .expect("append the replacement track");
    queue
        .run(move |q| q.select(replacement, Transition::None))
        .await
        .expect("a cleared queue selects its replacement");
    queue.run(kithara::queue::QueueControl::play).await;

    let position = wait_for_position_event(&mut rx, &queue, 0.2, Duration::from_secs(60))
        .await
        .expect("a track appended after clear must play, not sit silent at zero");
    assert!(
        position >= 0.2,
        "the replacement must advance past 0.2s, got {position}"
    );

    tick_handle.stop().await;
    queue.close().await;
}
