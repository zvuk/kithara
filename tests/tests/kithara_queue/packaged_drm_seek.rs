#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    assets::{FlushHub, FlushPolicy, StoreOptions},
    bufpool::{BytePool, PcmPool},
    decode::DecoderBackend,
    events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, sleep, timeout},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{config::AppConfig, sources::build_source};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64,
    fixture_protocol::{DelayRule, EncryptionRequest},
    hls_fixture::{aes128_iv, aes128_key_bytes},
    kithara,
    offline::OfflineSession,
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
                    return Err(format!("track failed: {err}"));
                }
            }
            _ => {}
        }
    }
    Err(format!("timeout waiting for {target:?}"))
}

/// Local mirror of the `track_plays_end_to_end` e2e seek scenario against
/// `assets/drm` (byte-identical to the silvercomet `/drm/` stream): load →
/// play past 0.5s → three seed-42 seeks, each of which must land near the
/// target AND resume audible progress — the exact sequence that hung on
/// the real CDN with `AbrMode::manual(2)`.
///
/// Track construction goes through the same `kithara-app` `build_source`
/// path as the e2e (shared downloader / flush hub / byte pool / asset
/// store from `AppConfig`), so the only axis left between this test and
/// the e2e is the network. Data and uniform latency alone do NOT
/// reproduce the production stall — that needs a mid-body network stall,
/// pinned separately by `playlist_stall_fails_load` and the kithara-net
/// `*_when_body_stalls` tests. This mirror pins the healthy-path contract
/// on identical data across decoder/ABR-mode axes.
async fn run_drm_seek(backend: DecoderBackend, abr: AbrMode, temp: TestTempDir) {
    install_tracing();

    let helper = TestServerHelper::new().await;
    let url = helper.asset("drm/master.m3u8");
    run_seek_scenario(&url, backend, abr, temp).await;
}

/// Same scenario against a generated encrypted fixture mirroring the
/// `assets/drm` geometry (37 × 6s segments, AES-128, 3 AAC variants) with a
/// per-segment fetch delay modelling CDN RTT. The delay is engine-backed
/// (virtual) under flash, so the "seek target segment is not yet available"
/// window the real CDN opens is reproduced deterministically.
async fn run_delayed_drm_seek(
    backend: DecoderBackend,
    abr: AbrMode,
    delay_ms: u64,
    temp: TestTempDir,
) {
    install_tracing();

    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(37)
        .segment_duration_secs(6.0)
        .variant_bandwidths(vec![66_005, 134_107, 269_930])
        .packaged_audio_aac_lc(44_100, 2)
        .encryption(EncryptionRequest {
            key_hex: hex::encode(aes128_key_bytes()),
            iv_hex: Some(hex::encode(aes128_iv())),
        })
        .push_delay_rule(DelayRule {
            delay_ms,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create delayed encrypted HLS fixture");
    let url = created.master_url();
    run_seek_scenario(&url, backend, abr, temp).await;
}

async fn run_seek_scenario(url: &Url, backend: DecoderBackend, abr: AbrMode, temp: TestTempDir) {
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(net, CancelToken::never()))
            .build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let config = AppConfig::new(
        downloader,
        flush_hub,
        CancelToken::never(),
        BytePool::default(),
        PcmPool::default(),
    );

    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let source = match build_source(url.as_str(), &config) {
        TrackSource::Config(mut cfg) => {
            cfg.store = StoreOptions::new(temp.path());
            cfg.decoder.backend = backend;
            cfg.initial_abr_mode = abr;
            TrackSource::Config(cfg)
        }
        other => other,
    };

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
    .unwrap_or_else(|e| panic!("load fail: {e}"));

    queue.select(id, Transition::None).expect("select");
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

    tick_handle.abort();
    queue.remove(id).expect("remove");
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
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    run_drm_seek(backend, abr, temp_dir).await;
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
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    run_drm_seek(backend, abr, temp_dir).await;
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
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
    temp_dir: TestTempDir,
) {
    run_delayed_drm_seek(backend, abr, 150, temp_dir).await;
}
