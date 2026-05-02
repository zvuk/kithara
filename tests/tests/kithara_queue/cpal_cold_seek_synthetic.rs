//! Cold-cache mid-track HLS seek through the full Queue → `PlayerImpl`
//! pipeline against the synthetic `TestServerHelper` HLS fixture.
//!
//! Same shape as `cold_seek_cpal::cpal_cold_seek_silvercomet_hls` but
//! against an in-process fixture and the offline session backend, so it
//! runs in every `just test` (no real network, no real audio device).
//! `cpal_cold_seek_silvercomet_hls` keeps the live-CDN + cpal repro
//! variant under `#[ignore]`/e2e for hang investigations.

#![forbid(unsafe_code)]

use std::{
    sync::{Arc, Once},
    time::{Duration, Instant},
};

use kithara_assets::StoreOptions;
use kithara_decode::DecoderBackend;
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, fixture_protocol::DelayRule, kithara, temp_dir,
};
use tokio::time::sleep;

static INIT_OFFLINE: Once = Once::new();

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: kithara_events::TrackId,
    deadline: Duration,
) -> Result<(), String> {
    use kithara_events::TrackStatus;
    let start = Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => return Err(format!("track failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "timeout after {deadline:?} (last={:?})",
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
) -> Result<f64, String> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Some(pos) = queue.position_seconds()
            && pos >= min_secs
        {
            return Ok(pos);
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err(format!(
        "position never reached {min_secs:.2}s (last={:?})",
        queue.position_seconds()
    ))
}

/// Cold-cache seek into a far segment over the offline backend.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn cold_seek_far_segment_hls_offline(#[case] backend: DecoderBackend) {
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    // 3 variants × 40 segments × 4s = 160s — closer to a real
    // 2–3 minute multi-bitrate HLS master. 200ms delay per segment
    // simulates cold-CDN latency so the ABR controller's decisions
    // have real wall-clock impact and a seek far ahead of the
    // current play head must actually drive the scheduler to fetch
    // fresh segments on the selected variant.
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
    let downloader = Downloader::new(DownloaderConfig::default());

    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(16)).await; // iced subscription cadence
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let mut cfg = ResourceConfig::new(master.as_str()).expect("valid master URL");
    cfg = cfg.with_downloader(downloader.clone());
    cfg.store = store;
    cfg.decoder_backend = backend;
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

    // Seek to 75% of the track — well past whatever the initial
    // fetch-ahead window covered. Matches the user's "seek to middle
    // of an uncached track".
    let seek_target = 120.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[offline] seek issued target={seek_target:.1}s (of 160s)");

    let observation_deadline = Instant::now() + Duration::from_secs(60);
    let mut confirmed = false;
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
        sleep(Duration::from_millis(200)).await;
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
