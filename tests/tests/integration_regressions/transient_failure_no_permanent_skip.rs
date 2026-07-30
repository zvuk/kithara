#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    events::{AudioEvent, DownloaderEvent, Event, QueueEvent, TrackId},
    hls::AbrMode,
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, PrivateTestServer, TestTempDir,
    audio_fixture::EmbeddedAudio,
    kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_event, wait_for_loader_done_event, wait_for_position_event},
};

/// Bounded look-ahead over the 222 s packaged fixture: the downloader must
/// still need further segments when the blip lands, otherwise the track is
/// already fully cached and no fetch can fail. Kept narrow for the same
/// reason as in the outage trap — a wide window makes the scenario depend on
/// how much happened to be cached.
const LOOK_AHEAD_BYTES: u64 = 64 * 1024;
const PLAY_BEFORE_FAILURE_SECS: f64 = 1.0;
/// How far playback must carry on past the blip. Bounds the "no auto-skip"
/// window with a fact rather than a stopwatch.
const MIN_RECOVERY_PROGRESS_SECS: f64 = 5.0;

struct NetworkRestore<'a>(&'a PrivateTestServer);

impl Drop for NetworkRestore<'_> {
    fn drop(&mut self) {
        self.0.set_network_online(true);
    }
}

fn spawn_ticker(queue: Arc<Queue>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(20)).await;
            if queue.tick().is_err() {
                break;
            }
        }
    })
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn transient_failure_does_not_kill_the_track(temp_dir: TestTempDir) {
    // A private server: the blip below takes every data route down, so sharing
    // one with parallel siblings would fail them instead.
    let server = PrivateTestServer::start().await;
    let helper = server.helper();
    let target_url = helper.asset("hls/master.m3u8");
    let fallback_fixture = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(EmbeddedAudio::TEST_MP3_BYTES.to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let fallback_url = fallback_fixture.child_url("fallback.mp3");

    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(500))
        .retry_policy(RetryPolicy::new(
            3,
            Duration::from_millis(10),
            Duration::from_millis(200),
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

    let target = queue.append(TrackSource::Config(Box::new(
        ResourceConfig::for_src(target_url.as_str())
            .expect("valid HLS URL")
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .downloader(downloader.clone())
            .initial_abr_mode(AbrMode::manual(0))
            .look_ahead_bytes(LOOK_AHEAD_BYTES)
            .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
            .build(),
    )));
    // A next track is what an auto-skip would move to. Without it the queue
    // has nowhere to go and the regression could not show itself.
    let fallback = queue.append(TrackSource::Config(Box::new(
        ResourceConfig::for_src(fallback_url.as_str())
            .expect("valid fallback URL")
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .downloader(downloader)
            .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
            .build(),
    )));

    let ticker = spawn_ticker(Arc::clone(&queue));
    let mut rx = queue.subscribe();
    queue
        .select(target, Transition::None)
        .expect("select target track");
    wait_for_loader_done_event(&mut rx, &queue, target, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("precondition: target load failed: {error}"));
    queue.play();

    let before_failure = wait_for_position_event(
        &mut rx,
        &queue,
        PLAY_BEFORE_FAILURE_SECS,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    // A blip, not an outage: the server goes away only long enough for one
    // in-flight segment fetch to fail, then comes straight back.
    server.set_network_online(false);
    let restore = NetworkRestore(&server);
    wait_for_event(
        &mut rx,
        "a segment fetch failing against the offline server",
        |event| {
            matches!(
                event,
                Event::Downloader(
                    DownloaderEvent::FirstByte { status: 503, .. }
                        | DownloaderEvent::RequestFailed { .. }
                        | DownloaderEvent::RetryExhausted { .. }
                )
            )
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "precondition: {error}; no fetch was in flight when the server went \
             away, so no transient failure was injected"
        )
    });
    drop(restore);

    let recovery_target = before_failure + MIN_RECOVERY_PROGRESS_SECS;
    let mut skipped_to: Option<TrackId> = None;
    let outcome = wait_for_event(
        &mut rx,
        "playback carrying on past the transient failure",
        |event| match event {
            Event::Queue(QueueEvent::TrackLoadFailed {
                id,
                auto_skipped: true,
                ..
            }) if *id == target => {
                skipped_to = Some(target);
                true
            }
            Event::Queue(QueueEvent::CurrentTrackChanged { id: Some(id) }) if *id == fallback => {
                skipped_to = Some(fallback);
                true
            }
            Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. }) => {
                *position_ms as f64 / 1000.0 >= recovery_target
            }
            _ => false,
        },
        Duration::from_secs(60),
    )
    .await;

    assert!(
        skipped_to.is_none(),
        "a transient failure at {before_failure:.3}s permanently failed the current \
         track and the queue auto-skipped away from it"
    );
    outcome.unwrap_or_else(|error| {
        panic!(
            "the track survived a transient failure at {before_failure:.3}s but \
             playback never reached {recovery_target:.3}s: {error}"
        )
    });

    queue.clear();
    ticker.abort();
    let _ = ticker.await;
}
