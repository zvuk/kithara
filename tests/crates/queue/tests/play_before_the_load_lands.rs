#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, TrackSource},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    TestServerHelper, kithara,
    offline::{OfflineQueue, QueueTicker},
    served_mp3, temp_dir,
    waits::wait_for_position_event,
};
use url::Url;

use crate::bufpool_ext::pools;

/// `play()` issued before the current track's load has landed must still
/// start that track once the load completes.
///
/// The ordering is structural, not timed: `append` only *spawns* the
/// loader task, and this test runs on a current-thread runtime, so the
/// loader provably has not executed a single step when `play()` is called
/// on the next line — there is no `.await` in between for it to run on.
///
/// Before the fix `play()` found an empty item slot, planted nothing in
/// the processor, and dropped the intent: when the load landed there was
/// no record that anybody had asked to play, so the track sat loaded and
/// silent forever. The first player in a process only escaped this because
/// the very first `start_stream` blocks `play()` long enough for the load
/// to win the race.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn play_issued_before_the_load_lands_still_starts_the_track(
    #[future(awt)] served_mp3: (TestServerHelper, Url),
) {
    let (_helper, url) = served_mp3;

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let session_pools = pools();
    let session = HostConfig::offline(session_pools.clone())
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(session_pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        session,
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
    )
    .await
    .expect("create product offline queue");
    let queue_for_tick = queue.control();
    let mut tick_handle = QueueTicker::spawn(queue_for_tick, Duration::from_millis(50));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools(),
            CancelToken::never(),
        ))
        .build(),
    );
    let cfg = ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid fixture URL"))
        .downloader(downloader)
        .store(store)
        .build();

    let mut rx = queue.subscribe();
    queue
        .run(move |q| q.append(TrackSource::Config(Box::new(cfg))))
        .await
        .expect("append play-before-load track");
    queue.run(move |q| q.play()).await;

    let position = wait_for_position_event(&mut rx, &queue, 0.2, Duration::from_secs(60))
        .await
        .expect(
            "a play() issued while the current track was still loading must start that track \
             once its load lands",
        );
    assert!(
        position >= 0.2,
        "playback must advance past 0.2s, got {position}"
    );

    tick_handle.stop().await;
    queue.close().await;
}
