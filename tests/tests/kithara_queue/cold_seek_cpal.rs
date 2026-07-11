#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time,
        time::{Duration, Instant, timeout},
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{kithara, temp_dir, waits::wait_for_position_at_least};

fn install_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "kithara_queue=info,kithara_audio=info,kithara_hls=debug,kithara_stream=info",
            )
        }))
        .with_test_writer()
        .try_init();
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &Queue,
    id: TrackId,
    target: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    if let Some(entry) = queue.track(id)
        && entry.status == target
    {
        return Ok(());
    }
    let start = Instant::now();
    while start.elapsed() < deadline {
        match timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status })))
                if tid == id =>
            {
                if status == target {
                    return Ok(());
                }
                if let TrackStatus::Failed(err) = status {
                    return Err(format!("failed: {err}"));
                }
            }
            _ => {}
        }
    }
    Err("timeout".into())
}

/// Real-network reproduction against silvercomet's HLS — the exact
/// track the user seeks on in the GUI demo. Uses the production app
/// pipeline (cpal backend, shared Downloader, cold cache dir) with
/// only the iced window stripped off.
///
/// This is the test that actually matters: all synthetic `PackagedTestServer`
/// scenarios pass cleanly, but the user reports a hang on silvercomet.
/// If this test reproduces, we have a live repro that points at
/// silvercomet-specific HTTP / format behaviour rather than anything in
/// the kithara pipeline abstract.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(360)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
#[ignore = "real network + real cpal; run manually: \
    cargo test --test suite_e2e \
    kithara::queue::cold_seek_cpal::cpal_cold_seek_silvercomet_hls \
    -- --ignored --nocapture --test-threads=1"]
async fn cpal_cold_seek_silvercomet_hls(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    install_tracing();

    const URL: &str = "https://stream.silvercomet.top/hls/master.m3u8";

    let temp = temp_dir();
    let store = StoreOptions::new(temp.path());
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(net, CancelToken::never()))
            .build(),
    );

    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
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

    let cfg = ResourceConfig::for_src(URL)
        .expect("valid silvercomet URL")
        .downloader(downloader.clone())
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();
    let source = TrackSource::Config(Box::new(cfg));

    let mut rx = queue.subscribe();
    let id = queue.append(source);
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("silvercomet track load failed: {e}"));

    queue.select(id, Transition::None).expect("select");
    queue.play();

    let pos_before = wait_for_position_at_least(&queue, 2.0, Duration::from_secs(45))
        .await
        .expect("silvercomet track never played past 2s");
    eprintln!("[silvercomet] pre-seek pos={pos_before:.3}s");

    let duration = queue.duration_seconds().unwrap_or(240.0);
    let seek_target = duration * 0.5;
    eprintln!("[silvercomet] duration={duration:.1}s, seeking to {seek_target:.1}s (50%)");
    queue.seek(seek_target).expect("seek accepted");

    let observation_deadline = Instant::now() + Duration::from_secs(90);
    let mut confirmed = false;
    let mut last_pos_log = Instant::now();
    while Instant::now() < observation_deadline {
        if let Some(pos) = queue.position_seconds()
            && pos > seek_target + 0.5
        {
            confirmed = true;
            break;
        }
        if tick_handle.is_finished() {
            break;
        }
        if last_pos_log.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "[silvercomet] still observing: pos={:?} target={seek_target:.2}s",
                queue.position_seconds()
            );
            last_pos_log = Instant::now();
        }
        time::sleep(Duration::from_millis(200)).await;
    }

    if tick_handle.is_finished() {
        match tick_handle.await {
            Ok(()) => panic!("tick task exited without panic"),
            Err(e) => panic!("SEEK HANG REPRODUCED on silvercomet: {e}"),
        }
    }

    assert!(
        confirmed,
        "silvercomet cold seek to {seek_target:.2}s never advanced past target \
         over 90 s (pos_before={pos_before:.2}, last={:?}) — ETERNAL FREEZE",
        queue.position_seconds(),
    );

    let backward_target = duration * 0.25;
    eprintln!(
        "[silvercomet] backward seek: from {:?} to {backward_target:.1}s (25%)",
        queue.position_seconds()
    );
    queue.seek(backward_target).expect("backward seek accepted");

    let observation_deadline = Instant::now() + Duration::from_secs(90);
    let mut backward_confirmed = false;
    let mut last_pos_log = Instant::now();
    while Instant::now() < observation_deadline {
        if let Some(pos) = queue.position_seconds()
            && (backward_target - 0.5..=backward_target + 3.0).contains(&pos)
        {
            backward_confirmed = true;
            break;
        }
        if tick_handle.is_finished() {
            break;
        }
        if last_pos_log.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "[silvercomet] backward observing: pos={:?} target={backward_target:.2}s",
                queue.position_seconds()
            );
            last_pos_log = Instant::now();
        }
        time::sleep(Duration::from_millis(200)).await;
    }

    if tick_handle.is_finished() {
        match tick_handle.await {
            Ok(()) => panic!("tick task exited without panic"),
            Err(e) => panic!("BACKWARD SEEK HANG REPRODUCED on silvercomet: {e}"),
        }
    }

    assert!(
        backward_confirmed,
        "silvercomet backward seek to {backward_target:.2}s never landed in range \
         (last={:?}) — BACKWARD SEEK FREEZE",
        queue.position_seconds(),
    );

    tick_handle.abort();
    drop(queue);
    drop(downloader);
    drop(temp);
}
