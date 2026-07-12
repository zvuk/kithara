#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{AudioEvent, Event},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, time::Duration, tokio},
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::DelayRule,
    kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_loader_done, wait_for_position_at_least},
};

/// Cold-cache seek into a far segment over the offline backend.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn cold_seek_far_segment_hls_offline(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

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

    let temp = temp_dir();
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
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
            time::sleep(Duration::from_millis(16)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid master URL")
        .downloader(downloader.clone())
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();
    let source = TrackSource::Config(Box::new(cfg));

    let id = queue.append(source);
    wait_for_loader_done(&queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("load: {e}"));

    queue.select(id, Transition::None).expect("select");
    queue.play();

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
        match time::timeout(Duration::from_secs(60), events.recv()).await {
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
        match tick_handle.await {
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

    tick_handle.abort();
    drop(queue);
    drop(downloader);
    drop(temp);
}
