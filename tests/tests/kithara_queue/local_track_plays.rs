//! Local mirror of `track_plays_end_to_end` from `real_playlist.rs`.
//!
//! Runs the full Queue → `PlayerImpl` → `OfflineBackend` pipeline against
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

/// Poll-based "loader is done with this track" check.
///
/// We don't subscribe to the broadcast: `wait_for_queue_event` (used in
/// the playlist scenario for crossfade / `CurrentTrackChanged`) consumes
/// events as it scans, so any `TrackStatusChanged{Loaded}` that arrived
/// before its predicate match is dropped from the receiver. Polling
/// `Queue::track()` side-steps that ordering hazard — production code is
/// fine, the race lives only in the helpers.
///
/// Treat both `Loaded` and `Consumed` as success, because
/// `Queue::spawn_apply_after_load` flips the status straight from
/// `Loaded` to `Consumed` when a `pending_select` was queued for the
/// same track (via `select_item_with_crossfade` → `mark_consumed`).
/// On real silvercomet that transition is rare per-test; on synthetic
/// fixtures it always happens for the first track and any auto-advanced
/// neighbour.
async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => {
                    return Err(format!("track entered Failed: {err}"));
                }
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "timeout after {deadline:?} (last status: {:?})",
                queue.track(track_id).map(|e| e.status)
            ));
        }
        sleep(Duration::from_millis(50)).await;
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

    let track_id = queue.append(source);

    // (a) load
    wait_for_loader_done(&queue, track_id, Duration::from_secs(30))
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

async fn wait_for_queue_event<F>(
    rx: &mut EventReceiver,
    mut pred: F,
    deadline: Duration,
) -> Option<QueueEvent>
where
    F: FnMut(&QueueEvent) -> bool,
{
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    let res = timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Queue(ev)) if pred(&ev) => return Some(ev),
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    })
    .await;
    res.unwrap_or(None)
}

/// Mirror of `real_playlist::queue_playlist_behavior` against local fixtures.
///
/// Five-track playlist (mp3 → hls aac → hls aes → hls aac → mp3) drives
/// the production pipeline through pause/resume, seek, manual crossfade,
/// auto-advance and `QueueEnded`. Per-track failures are collected so DRM
/// or loader regressions surface as a structured panic instead of the
/// first bad entry killing the whole test.
///
/// Currently `#[ignore]` because the test exposes a real production hang:
/// after the first crossfade, `kithara_stream::stream::read` panics with
/// `no progress for 10s` when the next HLS+AES track tries to emit PCM,
/// auto-advance to the following track times out, and `QueueEnded` never
/// fires. Real silvercomet hides this because CDN latency serializes
/// per-track work; the synthetic fixture loads everything in <100 ms and
/// races the stream-side back-pressure / DRM cache contention.
///
/// The test-helper race (`wait_for_status` losing events to
/// `wait_for_queue_event`) is fixed (`wait_for_loader_done` polls
/// `Queue::track()` directly and accepts `Loaded | Consumed`); what
/// remains is the underlying engine bug.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[case::apple(DecoderBackend::Apple)]
#[case::android(DecoderBackend::Android)]
async fn local_queue_playlist_behavior(#[case] backend: DecoderBackend) {
    if backend.skip_if_unavailable() {
        return;
    }
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let kinds = [
        LocalSource::Mp3,
        LocalSource::HlsAac,
        LocalSource::HlsAacAes128,
        LocalSource::HlsAac,
        LocalSource::Mp3,
    ];
    let mut urls: Vec<Url> = Vec::with_capacity(kinds.len());
    for &k in &kinds {
        urls.push(build_fixture_url(k, &helper).await);
    }

    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    queue.set_crossfade_duration(2.0);

    let mut rx = queue.subscribe();
    let ids: Vec<TrackId> = urls
        .iter()
        .map(|u| {
            let mut cfg = ResourceConfig::new(u.as_str()).expect("valid fixture URL");
            cfg = cfg.with_downloader(downloader.clone());
            cfg.store = store.clone();
            cfg.prefer_hardware = backend.prefer_hardware();
            cfg.initial_abr_mode = AbrMode::Auto(None);
            queue.append(TrackSource::Config(Box::new(cfg)))
        })
        .collect();

    // (1) First track starts
    queue
        .select(ids[0], Transition::None)
        .expect("select first");
    wait_for_loader_done(&queue, ids[0], Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("first track load [{}]: {e}", urls[0]));
    wait_for_position_at_least(&queue, 2.0, Duration::from_secs(15))
        .await
        .expect("first track position");

    // (2) Pause / resume — position must hold then advance.
    let before_pause = queue.position_seconds().unwrap_or(0.0);
    queue.pause();
    sleep(Duration::from_secs(2)).await;
    let during_pause = queue.position_seconds().unwrap_or(0.0);
    assert!(
        (during_pause - before_pause).abs() < 0.5,
        "position drifted during pause: {before_pause:.2} → {during_pause:.2}"
    );
    queue.play();
    sleep(Duration::from_millis(500)).await;
    let after_resume = queue.position_seconds().unwrap_or(0.0);
    assert!(
        after_resume >= during_pause - 0.1,
        "resume reset position: {during_pause:.2} → {after_resume:.2}"
    );
    assert!(
        after_resume > during_pause,
        "resume didn't advance position: {during_pause:.2} → {after_resume:.2}"
    );

    // (3) Seek mid-track
    let duration_0 = queue.duration_seconds().expect("duration for first track");
    let seek_target = duration_0 * 0.4;
    queue.seek(seek_target).expect("seek");
    wait_for_position_near(&queue, seek_target, 1.0, Duration::from_secs(5))
        .await
        .expect("seek landed near target");

    // (4) Manual crossfade to track 1
    wait_for_loader_done(&queue, ids[1], Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("pre-crossfade: next track load [{}]: {e}", urls[1]));
    let xf_duration = queue.crossfade_duration();
    queue.advance_to_next(Transition::Crossfade);
    let started = wait_for_queue_event(
        &mut rx,
        |ev| matches!(ev, QueueEvent::CrossfadeStarted { .. }),
        Duration::from_secs(10),
    )
    .await
    .expect("CrossfadeStarted event");
    if let QueueEvent::CrossfadeStarted { duration_seconds } = started {
        assert!(
            (duration_seconds - xf_duration).abs() < 0.01,
            "crossfade duration mismatch: event={duration_seconds:.2} vs config={xf_duration:.2}"
        );
    }
    wait_for_queue_event(
        &mut rx,
        |ev| matches!(ev, QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == ids[1]),
        Duration::from_millis((f64::from(xf_duration) * 1000.0) as u64 + 5_000),
    )
    .await
    .expect("CurrentTrackChanged to track 1 after crossfade");

    // (5..N-1) Auto-advance through remaining tracks
    let mut per_track: Vec<(String, Result<(), String>)> = Vec::new();
    for i in 1..urls.len() {
        let url = urls[i].clone();
        let result: Result<(), String> =
            async {
                wait_for_loader_done(&queue, ids[i], Duration::from_secs(30))
                    .await
                    .map_err(|e| format!("load: {e}"))?;
                wait_for_position_at_least(&queue, 2.0, Duration::from_secs(15))
                    .await
                    .map_err(|e| format!("play: {e}"))?;

                if i + 1 < urls.len() {
                    let dur = queue
                        .duration_seconds()
                        .ok_or_else(|| "duration unknown".to_string())?;
                    let near_end = (dur - f64::from(xf_duration) - 2.0).max(0.0);
                    queue.seek(near_end).map_err(|e| format!("seek: {e}"))?;
                    wait_for_queue_event(
                    &mut rx,
                    |ev| matches!(
                        ev,
                        QueueEvent::CurrentTrackChanged { id: Some(id) } if *id == ids[i + 1]
                    ),
                    Duration::from_secs(20),
                )
                .await
                .ok_or_else(|| "timeout on auto-advance".to_string())?;
                }
                Ok(())
            }
            .await;
        per_track.push((url.to_string(), result));
    }

    // (N) Last track → seek near end → QueueEnded
    let last_result: Result<(), String> = async {
        let dur = queue
            .duration_seconds()
            .ok_or_else(|| "duration unknown".to_string())?;
        queue
            .seek((dur - 3.0).max(0.0))
            .map_err(|e| format!("seek: {e}"))?;
        wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::QueueEnded),
            Duration::from_secs(15),
        )
        .await
        .ok_or_else(|| "timeout on QueueEnded".to_string())?;
        Ok(())
    }
    .await;

    let mut fails: Vec<String> = per_track
        .iter()
        .filter_map(|(u, r)| r.as_ref().err().map(|e| format!("  - {u}: {e}")))
        .collect();
    if let Err(e) = &last_result {
        fails.push(format!(
            "  - [last:{}] QueueEnded: {e}",
            urls[urls.len() - 1]
        ));
    }
    if !fails.is_empty() {
        panic!(
            "local_queue_playlist_behavior: {} track(s) failed:\n{}",
            fails.len(),
            fails.join("\n")
        );
    }

    tick_handle.abort();
}
