#![forbid(unsafe_code)]

use kithara::{
    decode::DecoderBackend,
    events::{AudioEvent, Event},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::DelayRule,
    kithara,
    offline::{OfflineQueue, QueueTicker},
    temp_dir,
    test_defaults::Consts as Shared,
    waits::{wait_for_loader_done, wait_for_position_at_least},
};
use url::Url;

use crate::bufpool_ext::pools;

/// Cold-cache seek into a far segment over the offline backend.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn cold_seek_far_segment_hls_offline(
    #[case] backend: DecoderBackend,
    #[future(awt)] cold_hls: (TestServerHelper, Url),
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_helper, master) = cold_hls;

    let temp = temp_dir();
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools(),
            CancelToken::never(),
        ))
        .build(),
    );

    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build(),
    );
    let queue = OfflineQueue::new(
        HostConfig::offline(pools())
            .pacing(Duration::from_millis(10))
            .build(),
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .await
    .expect("create product offline queue");

    let queue_for_tick = queue.control();
    let mut tick_handle = QueueTicker::spawn(queue_for_tick, Duration::from_millis(16));

    let cfg =
        ResourceConfig::for_src(ResourceSrc::parse(master.as_str()).expect("valid master URL"))
            .downloader(downloader.clone())
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build();
    let source = TrackSource::Config(Box::new(cfg));

    let id = queue
        .run(move |q| q.append(source))
        .await
        .expect("append synthetic HLS track");
    wait_for_loader_done(&queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("load: {e}"));

    queue
        .run(move |q| q.select(id, Transition::None))
        .await
        .expect("select");
    queue.run(move |q| q.play()).await;

    let pos_before = wait_for_position_at_least(&queue, 1.5, Duration::from_secs(20))
        .await
        .expect("track never played past 1.5s");
    eprintln!("[offline] pre-seek pos={pos_before:.3}s");

    // Subscribe before seeking so no post-seek playback-progress event is missed.
    let mut events = queue.subscribe();

    let seek_target = 120.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[offline] seek issued target={seek_target:.1}s (of 160s)");

    // Wait on the real playback-progress state: the audio pipeline emits
    // `PlaybackProgress { position_ms }` as committed PCM output advances. The
    // seek landed once that committed position passes the target. The tick task
    // pumps `position_seconds`; if it panics (hang reproduced) the watchdog
    // branch below reports it. `time::timeout` is only a safety deadline here.
    let mut confirmed = false;
    while !tick_handle.is_finished() {
        match time::timeout(Duration::from_secs(60), events.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }))) => {
                let pos_secs = position_ms as f64 / 1000.0;
                if pos_secs > seek_target + 0.5 {
                    confirmed = true;
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    if tick_handle.is_finished() {
        match tick_handle.join().await {
            Ok(()) => panic!("tick task exited without panic"),
            Err(e) => panic!("seek watchdog panicked — HANG REPRODUCED: {e}"),
        }
    }

    assert!(
        confirmed,
        "cold seek to {seek_target:.2}s never advanced past target \
         (pos_before={pos_before:.2}, last={:?}) — silent hang",
        queue.position_seconds(),
    );

    tick_handle.stop().await;
    queue.close().await;
    drop(downloader);
    drop(temp);
}

#[kithara::fixture]
async fn cold_hls() -> (TestServerHelper, Url) {
    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(40)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000, 2_560_000, 5_120_000])
        .packaged_audio_aac_lc(44_100, 2)
        .push_delay_rule(DelayRule {
            delay_ms: 200,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create long HLS fixture");
    let master = created.master_url();
    (helper, master)
}
