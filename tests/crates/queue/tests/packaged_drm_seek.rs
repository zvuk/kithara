#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    decode::DecoderBackend,
    events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, Instant, timeout},
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, policy::DomainKeyPolicy},
    queue::{Queue, QueueConfig, QueueControl, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    pools::{AppPools, PoolsSection, build as app_pools},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, Xorshift64,
    fixture_protocol::DelayRule,
    kithara, mixed_codec_ladder_encrypted,
    offline::{OfflineQueue, QueueTicker},
    temp_dir,
    waits::{wait_for_position_at_least, wait_for_position_near},
};
use url::Url;

fn install_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("kithara_queue=debug,kithara_hls=debug,kithara_audio=debug")
        }))
        .with_test_writer()
        .try_init();
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &QueueControl<AppPools>,
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
        match timeout(Duration::from_millis(500), rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status })))
                if tid == id =>
            {
                if status == target {
                    return Ok(());
                }
                if let TrackStatus::Failed(err) = status {
                    return Err(format!("track failed: {err}"));
                }
            }
            _ => {}
        }
    }
    Err(format!("timeout waiting for {target:?}"))
}

/// Local mirror of the `track_plays_end_to_end` e2e seek scenario: load →
/// play past 0.5s → three seed-42 seeks, each of which must land near the
/// target AND resume audible progress — the exact sequence that hung on
/// the real CDN with `AbrMode::manual(2)`.
///
/// Variant 2 is the top AAC of the production ladder, not its top variant:
/// above it sits the FLAC one, which only the auto cases climb to.
///
/// Track construction goes through `app_track_source`, the suite's mirror
/// of the `kithara-app` `build_source` path used by the e2e (same shared
/// downloader / flush hub / pool region / asset store from `AppConfig`), so
/// the only axis left between this test and the e2e is the network. Data
/// and uniform latency alone do NOT
/// reproduce the production stall — that needs a mid-body network stall,
/// pinned separately by `playlist_stall_fails_load` and the kithara-net
/// `*_when_body_stalls` tests. This mirror pins the healthy-path contract
/// across decoder/ABR-mode axes.
async fn drm_source(delay_ms: Option<u64>) -> (TestServerHelper, Url) {
    let helper = TestServerHelper::new().await;
    let mut builder = mixed_codec_ladder_encrypted();
    if let Some(delay_ms) = delay_ms {
        builder = builder.push_delay_rule(DelayRule {
            delay_ms,
            ..DelayRule::default()
        });
    }
    let created = helper
        .create_hls(builder)
        .await
        .expect("create encrypted HLS fixture");
    let url = created.master_url();
    (helper, url)
}

#[kithara::fixture]
async fn drm_track() -> (TestServerHelper, Url) {
    drm_source(None).await
}

#[kithara::fixture]
async fn delayed_drm_track() -> (TestServerHelper, Url) {
    drm_source(Some(150)).await
}

async fn run_seek_scenario(url: &Url, backend: DecoderBackend, abr: AbrMode, temp: TestTempDir) {
    let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let shutdown = CancelToken::never();
    let store = AssetStore::builder(pools.clone())
        .cancel(shutdown.child())
        .backend(StorageBackend::default())
        .flush_hub(flush_hub)
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(shutdown.child())
            .build(),
    );
    let session_pools = worker.pools().clone();
    let config = AppConfig::builder()
        // The fixture serves its own AES-128 keys; no provider claims 127.0.0.1.
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker.clone())
        .store(store)
        .build();

    let session_config = HostConfig::offline(session_pools)
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session_config.sample_rate())
            .worker(worker)
            .build(),
    );
    let queue = OfflineQueue::new(
        session_config,
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .await
    .expect("create product offline queue");
    let mut tick_handle = QueueTicker::spawn(queue.control(), Duration::from_millis(50));

    let source = super::app_track_source(
        url.as_str(),
        &config,
        super::app_disk_asset_store(&config, temp.path()),
        backend,
        abr,
        None,
    );

    let mut rx = queue.subscribe();
    let id = queue
        .run(move |q| q.append(source))
        .await
        .expect("append packaged DRM track");
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load fail: {e}"));

    queue
        .run(move |q| q.select(id, Transition::None))
        .await
        .expect("select");
    wait_for_position_at_least(&queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("play fail: {e}"));

    let duration = queue
        .duration_seconds()
        .expect("duration known after Loaded");
    let mut rng = Xorshift64::new(42);
    for i in 0..3 {
        let target = duration * rng.range_f64(0.05, 0.95);
        queue.seek(target).expect("seek");
        wait_for_position_near(&queue, target, 1.0, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} to {target:.1}s fail: {e}"));
        let before = queue.position_seconds().unwrap_or(0.0);
        wait_for_position_at_least(&queue, before + 0.5, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} hang: {e}"));
        let after = queue.position_seconds().unwrap_or(0.0);
        assert!(
            after - before >= 0.5,
            "seek #{i} hang: {before:.2}→{after:.2}"
        );
    }

    queue.remove(id).expect("remove");
    tick_handle.stop().await;
    queue.close().await;
}

#[kithara::test(tokio)]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::manual(2))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_low(DecoderBackend::Apple, AbrMode::manual(0))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_high(DecoderBackend::Apple, AbrMode::manual(2))
)]
async fn drm_seek_resumes(
    #[future(awt)] drm_track: (TestServerHelper, Url),
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    let (_helper, url) = drm_track;
    install_tracing();
    run_seek_scenario(&url, backend, abr, temp_dir).await;
}

// flash(false): the e2e this mirrors runs real-clock; the stall window is
// timing-dependent, so the real-clock lane is the one expected to catch it.
#[kithara::test(tokio, flash(false))]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::manual(2))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_low(DecoderBackend::Apple, AbrMode::manual(0))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_high(DecoderBackend::Apple, AbrMode::manual(2))
)]
async fn drm_seek_resumes_realtime(
    #[future(awt)] drm_track: (TestServerHelper, Url),
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    let (_helper, url) = drm_track;
    install_tracing();
    run_seek_scenario(&url, backend, abr, temp_dir).await;
}

#[kithara::test(tokio)]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::manual(2))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_low(DecoderBackend::Apple, AbrMode::manual(0))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_high(DecoderBackend::Apple, AbrMode::manual(2))
)]
async fn drm_seek_resumes_delayed_cdn(
    #[future(awt)] delayed_drm_track: (TestServerHelper, Url),
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    let (_helper, url) = delayed_drm_track;
    install_tracing();
    run_seek_scenario(&url, backend, abr, temp_dir).await;
}
