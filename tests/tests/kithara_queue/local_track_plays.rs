//! Local mirror of `track_plays_end_to_end` from `real_playlist.rs`.
//!
//! Runs the full Queue → PlayerImpl → OfflineBackend pipeline against
//! `TestServerHelper` fixtures (raw MP3, packaged AAC HLS, packaged
//! AAC HLS+AES128) instead of silvercomet/zvuk live URLs. Shape of the
//! scenario is identical: load → play with monotonic progress → 3
//! random seeks with hang detection → position-consistency window.
//!
//! No `#[ignore]` — runs in every `just test` so seek/loader/HLS-DRM
//! regressions surface without the e2e gate.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    sync::{Arc, Once},
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, Event, EventReceiver, QueueEvent, TrackId, TrackStatus};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, Xorshift64,
    fixture_protocol::EncryptionRequest, kithara, temp_dir,
};
use tokio::time::{sleep, timeout};
use url::Url;

use crate::common::decoder_backend::DecoderBackend;

static INIT_OFFLINE: Once = Once::new();

#[derive(Clone, Copy, Debug)]
enum LocalSource {
    Mp3,
    HlsAac,
    HlsAacAes128,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").expect("hex write");
    }
    s
}

async fn build_fixture_url(kind: LocalSource, helper: &TestServerHelper) -> Url {
    match kind {
        LocalSource::Mp3 => helper.asset("track.mp3"),
        LocalSource::HlsAac => {
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2);
            helper
                .create_hls(builder)
                .await
                .expect("create local plain HLS fixture")
                .master_url()
        }
        LocalSource::HlsAacAes128 => {
            let key: &[u8] = b"0123456789abcdef";
            let iv: [u8; 16] = [0u8; 16];
            let builder = HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(16)
                .segment_duration_secs(4.0)
                .packaged_audio_aac_lc(44_100, 2)
                .encryption(EncryptionRequest {
                    key_hex: hex_encode(key),
                    iv_hex: Some(hex_encode(&iv)),
                });
            helper
                .create_hls(builder)
                .await
                .expect("create local encrypted HLS fixture")
                .master_url()
        }
    }
}

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    target: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    if let Some(entry) = queue.track(track_id) {
        if entry.status == target {
            return Ok(());
        }
        if let TrackStatus::Failed(err) = &entry.status {
            return Err(format!("track entered Failed: {err}"));
        }
    }
    let res = timeout(deadline, async {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            };
            if let Event::Queue(QueueEvent::TrackStatusChanged { id, status }) = ev
                && id == track_id
            {
                match &status {
                    s if *s == target => return Ok(()),
                    TrackStatus::Failed(err) => {
                        return Err(format!("track entered Failed: {err}"));
                    }
                    _ => continue,
                }
            }
        }
    })
    .await;
    match res {
        Ok(r) => r,
        Err(_) => Err(format!("timeout waiting for {target:?} after {deadline:?}")),
    }
}

async fn wait_for_position_at_least(
    queue: &Queue,
    min_secs: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(pos) = queue.position_seconds()
            && pos >= min_secs
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position stayed below {min_secs:.2}s for {deadline:?}"
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_position_near(
    queue: &Queue,
    target: f64,
    tolerance: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(pos) = queue.position_seconds()
            && (pos - target).abs() < tolerance
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position never reached {target:.2}s (±{tolerance:.2}) in {deadline:?}"
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn sample_positions(queue: &Queue, count: usize, interval: Duration) -> Vec<f64> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(queue.position_seconds().unwrap_or(0.0));
        sleep(interval).await;
    }
    out
}

fn assert_monotonic_nondecreasing(samples: &[f64], label: &str) {
    for w in samples.windows(2) {
        assert!(
            w[1] >= w[0] - 0.05,
            "position regressed [{label}]: {samples:?}"
        );
    }
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let downloader = Downloader::new(DownloaderConfig::default());
    let store = StoreOptions::new(temp_dir.path());
    (queue, downloader, store, tick_handle)
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::mp3_symphonia(LocalSource::Mp3, 42, DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::mp3_apple(LocalSource::Mp3, 42, DecoderBackend::Apple, AbrMode::Auto(None))]
#[case::mp3_android(LocalSource::Mp3, 42, DecoderBackend::Android, AbrMode::Auto(None))]
#[case::hls_aac_symphonia(
    LocalSource::HlsAac,
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[case::hls_aac_apple(LocalSource::HlsAac, 42, DecoderBackend::Apple, AbrMode::Auto(None))]
#[case::hls_aac_android(LocalSource::HlsAac, 42, DecoderBackend::Android, AbrMode::Auto(None))]
#[case::hls_aes_symphonia(
    LocalSource::HlsAacAes128,
    42,
    DecoderBackend::Symphonia,
    AbrMode::Auto(None)
)]
#[case::hls_aes_apple(
    LocalSource::HlsAacAes128,
    42,
    DecoderBackend::Apple,
    AbrMode::Auto(None)
)]
#[case::hls_aes_android(
    LocalSource::HlsAacAes128,
    42,
    DecoderBackend::Android,
    AbrMode::Auto(None)
)]
async fn local_track_plays_end_to_end(
    #[case] kind: LocalSource,
    #[case] rng_seed: u64,
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
) {
    if backend.skip_if_unavailable() {
        return;
    }
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let url = build_fixture_url(kind, &helper).await;
    let label = format!("{kind:?}/{backend:?}");

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let mut cfg = ResourceConfig::new(url.as_str()).expect("valid fixture URL");
    cfg = cfg.with_downloader(downloader.clone());
    cfg.store = store;
    cfg.prefer_hardware = backend.prefer_hardware();
    cfg.initial_abr_mode = abr;
    let source = TrackSource::Config(Box::new(cfg));

    let mut rx = queue.subscribe();
    let track_id = queue.append(source);

    // (a) load
    wait_for_status(
        &mut rx,
        &queue,
        track_id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load fail [{label}]: {e}"));

    // (b) play + monotonic progress
    queue.select(track_id, Transition::None).expect("select");
    wait_for_position_at_least(&queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("play fail [{label}]: {e}"));
    let progress = sample_positions(&queue, 5, Duration::from_millis(200)).await;
    assert_monotonic_nondecreasing(&progress, &label);

    // (c) deterministic seek × 3 random
    let duration = queue
        .duration_seconds()
        .expect("duration known after Loaded");
    let mut rng = Xorshift64::new(rng_seed);
    for i in 0..3 {
        let target = duration * rng.range_f64(0.05, 0.95);
        queue.seek(target).expect("seek");
        wait_for_position_near(&queue, target, 1.0, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} to {target:.1}s fail [{label}]: {e}"));
        let before = queue.position_seconds().unwrap_or(0.0);
        sleep(Duration::from_secs(2)).await;
        let after = queue.position_seconds().unwrap_or(0.0);
        assert!(
            after - before >= 0.5,
            "seek #{i} hang [{label}]: {before:.2}→{after:.2} over 2s"
        );
    }

    // (d) position consistency (offline backend best-effort realtime)
    let start_pos = queue.position_seconds().unwrap_or(0.0);
    sleep(Duration::from_secs(2)).await;
    let end_pos = queue.position_seconds().unwrap_or(0.0);
    let gain = end_pos - start_pos;
    assert!(
        (0.9..=2.5).contains(&gain),
        "position gain out of offline-realtime window [{label}]: got \
         {gain:.2}s over 2s wall clock (start={start_pos:.2} end={end_pos:.2})"
    );

    queue.remove(track_id).expect("remove");
    tick_handle.abort();
}
