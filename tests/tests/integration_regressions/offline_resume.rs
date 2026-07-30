#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    events::{AudioEvent, DownloaderEvent, Event},
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
    PrivateTestServer, TestTempDir, kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_event, wait_for_loader_done_event, wait_for_position_event},
};

/// Playback failing to resume once connectivity returns does not reproduce in
/// the core: with connectivity restored the engine resumes on its own. This
/// stays as the contract that keeps it that way, and as the alibi placing the
/// reported bug above the Rust layer.
/// Verified to have teeth — leaving the server offline pins playback at the
/// starvation point instead of advancing.
///
/// The packaged `hls/` fixture is a 37-segment, 222 s variant whose slq
/// segments are ~50 KiB. A track far longer than the look-ahead window is
/// what makes the outage observable at all: with a short fixture the
/// downloader finishes the whole variant over loopback before playback
/// reaches its first second, and taking the network down afterwards is a
/// no-op no product could react to.
///
/// The window is deliberately narrow: the outage only becomes observable once
/// the cached bytes drain, and draining is paced by real playback. At 256 KiB
/// the drain outlasted the wait on roughly one run in five.
const LOOK_AHEAD_BYTES: u64 = 64 * 1024;
const PLAY_BEFORE_OUTAGE_SECS: f64 = 1.0;
const MIN_RESUME_PROGRESS_SECS: f64 = 1.0;

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
async fn playback_resumes_after_network_returns(temp_dir: TestTempDir) {
    // A private server: this test takes the network down, and the switch covers
    // every data route on whichever server it runs against.
    let server = PrivateTestServer::start().await;
    let url = server.helper().asset("hls/master.m3u8");

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
    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("valid HLS URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader)
        .initial_abr_mode(AbrMode::manual(0))
        .look_ahead_bytes(LOOK_AHEAD_BYTES)
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
        .build();

    let ticker = spawn_ticker(Arc::clone(&queue));
    let mut rx = queue.subscribe();
    let id = queue.append(TrackSource::Config(Box::new(cfg)));
    queue
        .select(id, Transition::None)
        .expect("select HLS track");
    wait_for_loader_done_event(&mut rx, &queue, id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|error| panic!("precondition: {error}"));
    queue.play();

    let before_outage = wait_for_position_event(
        &mut rx,
        &queue,
        PLAY_BEFORE_OUTAGE_SECS,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    server.set_network_online(false);
    let _network_restore = NetworkRestore(&server);
    wait_for_event(
        &mut rx,
        "a segment fetch observing the offline server",
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
            "precondition: {error}; the look-ahead window never needed a further \
             segment, so no outage was ever observed"
        )
    });

    // Resumption is only meaningful once playback has genuinely run dry.
    // Without this the buffered look-ahead carries the position past the
    // resume target on its own and the assertion below passes vacuously.
    let mut starved_at = 0.0;
    wait_for_event(
        &mut rx,
        "playback starving on the exhausted buffer",
        |event| {
            let Event::Audio(AudioEvent::UnderrunStarted { position_ms, .. }) = event else {
                return false;
            };
            starved_at = *position_ms as f64 / 1000.0;
            true
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "precondition: {error}; the buffer never ran dry while the network was \
             down, so there was nothing for the recovery to resume from"
        )
    });
    server.set_network_online(true);

    let resume_target = starved_at + MIN_RESUME_PROGRESS_SECS;
    let resumed_at =
        wait_for_position_event(&mut rx, &queue, resume_target, Duration::from_secs(30))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "connectivity returned after starving at {starved_at:.3}s (outage began at {before_outage:.3}s), but \
                 playback never reached {resume_target:.3}s: {error}"
                )
            });
    assert!(
        resumed_at >= resume_target,
        "playback stayed at {resumed_at:.3}s after the network returned \
         (starved at {starved_at:.3}s, outage began at {before_outage:.3}s)"
    );

    queue.clear();
    ticker.abort();
    let _ = ticker.await;
}
