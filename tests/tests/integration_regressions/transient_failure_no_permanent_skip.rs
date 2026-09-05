#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::{AudioEvent, DownloaderEvent, Event, QueueEvent, TrackId},
    hls::{AbrMode, HlsConfigPatch},
    host::HostConfig,
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, HlsFixtureBuilder, PrivateTestServer, TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara,
    offline::OfflineQueue,
    temp_dir,
    test_defaults::Consts as Shared,
    waits::{wait_for_event, wait_for_loader_done_event, wait_for_position_event},
};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;

fn hls_look_ahead(bytes: u64) -> HlsConfigPatch {
    let mut patch = HlsConfigPatch::default();
    patch.look_ahead_bytes = Some(bytes);
    patch
}

/// The ladder the blip lands on. One variant, since playback pins variant 0
/// below, and 72 s of it: the downloader must still need further segments
/// when the network drops, or the track is already fully cached and no fetch
/// can fail.
const VARIANT_SEGMENTS: usize = 24;
const SEGMENT_SECS: f64 = 3.0;
/// Bounded look-ahead: kept narrow because a wide window makes the scenario
/// depend on how much happened to be cached. At the encoder's default
/// 128 kbit/s a segment above runs roughly 48 KiB, so this window holds
/// about one of them — the ratio the captured tree gave, where a 64 KiB
/// window sat against ~50 KiB segments.
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

fn spawn_ticker(queue: QueueControl<TestPools>) -> tokio::task::JoinHandle<()> {
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
    let target = helper
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(VARIANT_SEGMENTS)
                .segment_duration_secs(SEGMENT_SECS)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create the ladder the blip lands on");
    let target_url = target.master_url();
    let fallback_fixture = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
            content_type: Some("audio/mpeg"),
        },
        delivery: Delivery::Range,
    });
    let fallback_url = fallback_fixture.child_url("fallback.mp3");

    let pools = pools();
    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(500))
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(3)
                .base_delay(Duration::from_millis(10))
                .max_delay(Duration::from_millis(200))
                .build(),
        )
        .build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::new(
        HostConfig::offline(pools)
            .pacing(Duration::from_millis(10))
            .build(),
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
    )
    .expect("create product offline queue");

    let target = queue
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(
                ResourceSrc::parse(target_url.as_str()).expect("valid HLS URL"),
            )
            .downloader(downloader.clone())
            .initial_abr_mode(AbrMode::manual(0))
            .hls(hls_look_ahead(LOOK_AHEAD_BYTES))
            .store(store.clone())
            .build(),
        )))
        .expect("append target track");
    // A next track is what an auto-skip would move to. Without it the queue
    // has nowhere to go and the regression could not show itself.
    let fallback = queue
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(
                ResourceSrc::parse(fallback_url.as_str()).expect("valid fallback URL"),
            )
            .downloader(downloader)
            .store(store)
            .build(),
        )))
        .expect("append fallback track");

    let ticker = spawn_ticker(queue.control());
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
