#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, time::Duration, tokio},
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    TestServerHelper, kithara, offline::OfflineSession, temp_dir, waits::wait_for_position_event,
};

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
async fn play_issued_before_the_load_lands_still_starts_the_track() {
    let helper = TestServerHelper::new().await;
    let url = helper.asset("track.mp3");

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(
        QueueConfig::default()
            .with_player(player)
            .with_store(store.clone()),
    ));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader)
        .store(store)
        .build();

    let mut rx = queue.subscribe();
    queue.append(TrackSource::Config(Box::new(cfg)));
    queue.play();

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

    tick_handle.abort();
}
