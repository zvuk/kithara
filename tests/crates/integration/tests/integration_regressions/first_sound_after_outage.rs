#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    download::{Downloader, DownloaderConfig, DownloaderEvent},
    hls::{AbrMode, HlsConfigPatch},
    host::HostConfig,
    net::{HttpClient, NetOptions, RetryPolicy},
    platform::{CancelToken, sync::Arc, time::Duration, tokio},
    play::{PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueEvent, TrackSource},
};
use kithara_integration_tests::{
    PrivateTestServer, TestTempDir,
    bufpool_ext::pools,
    disk_asset_store,
    event::TestEvent,
    kithara,
    offline::{OfflineQueue, QueueTicker},
    temp_dir,
    test_defaults::Consts as Shared,
    test_server::NetworkMode,
    waits::{wait_for_event, wait_for_position_event},
};

/// Wide enough that the first segments are wanted immediately, so the outage is
/// observed on the very first fetches rather than after a cached prefix.
const LOOK_AHEAD_BYTES: u64 = 1024 * 1024;
/// Any audible progress proves the track started; the point is that it started
/// at all, not how far it ran.
const MIN_FIRST_SOUND_SECS: f64 = 1.0;

struct NetworkRestore<'a>(&'a PrivateTestServer);

impl Drop for NetworkRestore<'_> {
    fn drop(&mut self) {
        self.0.set_network_mode(NetworkMode::Online);
    }
}

#[kithara::fixture]
async fn first_sound_source() -> (PrivateTestServer, String) {
    let server = PrivateTestServer::start().await;
    let url = server.helper().asset("hls/master.m3u8").to_string();
    (server, url)
}

/// An outage that starts before the first sound must not cost the track its
/// only attempt.
///
/// This is the other half of LABA-419 and a different path from
/// `offline_resume`: there, playback is already running, so a starved segment
/// re-enters the plan and the reader is there to ask for it again. Here nothing
/// is playing yet — the load itself fails, so there is no reader, no ring and no
/// underrun. The track must still be playable once connectivity returns,
/// without the user selecting it a second time.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn first_sound_arrives_after_an_outage_before_playback(
    temp_dir: TestTempDir,
    #[future(awt)] first_sound_source: (PrivateTestServer, String),
) {
    let (server, url) = first_sound_source;

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
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::paced(
        HostConfig::offline(pools).build(),
        Queue::new(QueueConfig::builder().player(player).build()),
        Duration::from_millis(10),
    )
    .await
    .expect("create product offline queue");
    let mut hls = HlsConfigPatch::default();
    hls.look_ahead_bytes = Some(Some(LOOK_AHEAD_BYTES));
    let cfg = ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid HLS URL"))
        .downloader(downloader)
        .initial_abr_mode(AbrMode::manual(0))
        .hls(hls)
        .store(disk_asset_store(temp_dir.path()))
        .build();

    // The network refuses before anything is asked for, so the track never
    // reaches a first byte.
    //
    // The refusal is the reachable-server class, not a severed transport, and
    // that is deliberate: what buries the track here is the load failing at
    // all, and both classes arrive at the queue the same way — as a failure the
    // downloader itself reported. A severed transport cannot be used on this
    // path because the playlist body is drained by its consumer, so the outage
    // surfaces as an unreported hang rather than a failure (see
    // `playlist_stall_fails_load` for the contract that path does hold).
    server.set_network_mode(NetworkMode::Unavailable);
    let _network_restore = NetworkRestore(&server);

    let mut ticker = QueueTicker::spawn(queue.control(), Duration::from_millis(20));
    let mut rx = queue.subscribe();
    queue
        .run(move |q| q.append(TrackSource::Config(Box::new(cfg))))
        .await
        .expect("append first-sound track");
    queue.run(|q| q.play()).await;

    // The outage reached the load when a request it issued spent its budget
    // against the refusal. Not "the track was marked failed": that verdict is
    // the defect under test, so a precondition resting on it would evaporate
    // with the fix.
    wait_for_event(
        &mut rx,
        "the track load observing the outage",
        |event| {
            matches!(
                event,
                TestEvent::Downloader(
                    DownloaderEvent::RetryExhausted { .. } | DownloaderEvent::RequestFailed { .. }
                )
            )
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "precondition: {error}; the load never observed the outage, so there \
             was nothing for the recovery to retry"
        )
    });

    server.set_network_mode(NetworkMode::Online);

    // What the recovery did with the track, kept for the failure message: a load
    // that never succeeded and a load that succeeded into silence are different
    // defects, and the position alone cannot tell them apart.
    let history = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorder = {
        let history = Arc::clone(&history);
        let mut rx = queue.subscribe();
        tokio::task::spawn(async move {
            while let Ok(event) = rx.recv().await {
                match event.event {
                    TestEvent::Queue(QueueEvent::TrackStatusChanged { id, status, .. }) => history
                        .lock()
                        .expect("history")
                        .push(format!("{id:?} -> {status:?}")),
                    TestEvent::Downloader(DownloaderEvent::RequestFailed { error, .. }) => history
                        .lock()
                        .expect("history")
                        .push(format!("request failed: {error}")),
                    TestEvent::Downloader(DownloaderEvent::RetryExhausted { error, .. }) => history
                        .lock()
                        .expect("history")
                        .push(format!("retries spent: {error}")),
                    _ => {}
                }
            }
        })
    };

    let started_at = wait_for_position_event(
        &mut rx,
        &queue,
        MIN_FIRST_SOUND_SECS,
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|error| {
        let seen = history.lock().expect("history").join("\n  ");
        panic!(
            "connectivity returned, but the track never reached \
             {MIN_FIRST_SOUND_SECS:.3}s on its own: {error}\nafter the network came back:\n  {seen}"
        )
    });
    recorder.abort();
    assert!(
        started_at >= MIN_FIRST_SOUND_SECS,
        "the track stalled at {started_at:.3}s after the network returned"
    );

    queue.run(|q| q.clear()).await;
    ticker.stop().await;
    queue.close().await;
}
