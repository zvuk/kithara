//! Reproduces the user-reported cold-cache mid-track seek hang against
//! a local `PackagedTestServer` — the whole production pipeline (Queue,
//! PlayerImpl, OfflineBackend) end-to-end, no real network.
//!
//! Scenario: two HLS tracks, caller selects the second, plays briefly,
//! then seeks to the middle of an uncached range. The test asserts the
//! queue's watchdog either confirms progress or fires its panic+dump.
//! If the hang reproduces, `Queue::tick` panics through
//! `HangDetector`; the panic payload + `/tmp/kithara-seek-hang-*.json`
//! pinpoint the frozen state.

#![forbid(unsafe_code)]

use std::{
    sync::{Arc, Once},
    time::{Duration, Instant},
};

use kithara_assets::StoreOptions;
use kithara_events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, PackagedTestServer, TestServerHelper, TestTempDir,
    fixture_protocol::DelayRule, kithara, temp_dir,
};
use tokio::time::sleep;

static INIT_OFFLINE: Once = Once::new();

fn install_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("kithara_queue=debug,kithara_hls=debug,kithara_stream=debug")
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
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
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
        "position never reached {min_secs:.2}s in {:?} (last={:?})",
        deadline,
        queue.position_seconds()
    ))
}

async fn run_seek_scenario(urls: &[&str], select_index: usize, temp: TestTempDir) {
    install_tracing();
    INIT_OFFLINE.call_once(init_offline_backend);

    let server = PackagedTestServer::new().await;
    let resolved: Vec<String> = urls
        .iter()
        .map(|p| server.url(p).as_str().to_string())
        .collect();

    // `temp` is the shared `temp_dir` fixture — dropped (auto-deleted)
    // at the end of this function, so the shared app cache at
    // `env::temp_dir()/kithara` stays untouched.
    let store = StoreOptions::new(temp.path());
    let downloader = Downloader::new(DownloaderConfig::default());

    let player = Arc::new(PlayerImpl::new(PlayerConfig::default()));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    // Background tick driver — production uses the UI loop for this; in
    // tests we spawn a dedicated task so `Queue::tick` runs continuously
    // and the seek watchdog can observe progress (or lack thereof).
    let queue_for_tick = Arc::clone(&queue);
    let tick_handle = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if queue_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let mut rx = queue.subscribe();
    let ids: Vec<TrackId> = resolved
        .iter()
        .map(|u| {
            let mut cfg = ResourceConfig::new(u).expect("valid URL");
            cfg = cfg.with_downloader(downloader.clone());
            cfg.store = store.clone();
            queue.append(TrackSource::Config(Box::new(cfg)))
        })
        .collect();
    let selected_id = ids[select_index];

    wait_for_status(
        &mut rx,
        &queue,
        selected_id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("selected track never reached Loaded: {e}"));

    queue.select(selected_id, Transition::None).expect("select");
    queue.play();

    let pos_before_seek = wait_for_position_at_least(&queue, 1.0, Duration::from_secs(15))
        .await
        .expect("selected track never played past 1s");

    // Seek into the middle of the (uncached) track. Test fixture has
    // 3 segments × 4s = 12s total, so 6.0s lands in segment 1 which
    // the decoder has not necessarily fetched yet.
    let seek_target = 6.0;
    assert!(
        seek_target > pos_before_seek + 2.0,
        "seek target must be ahead of current play head (pos={pos_before_seek:.2}, target={seek_target:.2})"
    );
    queue.seek(seek_target).expect("seek accepted by player");

    // Give the queue watchdog a generous observation window. If the
    // HLS pipeline hangs, the watchdog panics with a state dump and
    // this `await` panics inside `Queue::tick` on the spawned task —
    // but tokio surfaces it on the join below.
    //
    // Poll position progress here too, so that a successful seek
    // exits the test quickly instead of waiting for the full window.
    let observation_deadline = Instant::now() + Duration::from_secs(15);
    let mut confirmed = false;
    while Instant::now() < observation_deadline {
        if let Some(pos) = queue.position_seconds()
            && pos > seek_target + 0.5
        {
            confirmed = true;
            break;
        }
        // Surface tick-task panic if it happened.
        if tick_handle.is_finished() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    // If the tick task crashed (watchdog panic), propagate it.
    if tick_handle.is_finished() {
        match tick_handle.await {
            Ok(()) => panic!("tick task exited unexpectedly without panic"),
            Err(e) => panic!("seek watchdog panicked (expected on bug reproduction): {e}"),
        }
    }

    assert!(
        confirmed,
        "seek to {seek_target:.2}s did not produce audible progress within 15s \
         (pre-seek pos={pos_before_seek:.2}, last pos={:?}) \
         — either the hang was silent (watchdog budget too long) or the pipeline \
         recovered too slowly to be distinguishable from a real stall",
        queue.position_seconds()
    );

    tick_handle.abort();
    drop(queue);
    let _ = ids;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_one_track_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8"], 0, temp_dir).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_two_tracks_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master-encrypted.m3u8"], 0, temp_dir).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn queue_seek_two_tracks_index1(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master-encrypted.m3u8"], 1, temp_dir).await;
}

/// Two concurrent `Queue::append` calls for the exact same HLS URL
/// must both succeed. A cold-cache race between the two in-flight
/// downloads corrupts the playlist body — one loader starts writing
/// segment bytes into the file the other is reading as the master
/// playlist, producing `Playlist parsing error: expected #EXTM3U at
/// the start of "\0\0..."`.
///
/// Fix is a proper downloader-level request coalescer (single-flight
/// layer between `PeerHandle::execute` and the HTTP client) — design
/// note at `.docs/plans/2026-04-21-downloader-request-coalescing.md`.
/// Marked `#[ignore]` until the coalescer lands so the work gap is
/// visible without blocking `just test`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "requires downloader request coalescing — see \
    .docs/plans/2026-04-21-downloader-request-coalescing.md"]
async fn queue_seek_same_url_twice_index0(temp_dir: TestTempDir) {
    run_seek_scenario(&["/master.m3u8", "/master.m3u8"], 0, temp_dir).await;
}

/// Long-track variant: 20 segments × 4s = 80s, with a 150ms delay on
/// every segment fetch to emulate a cold-network CDN. Seeks past the
/// initial fetched window, into a segment that has to be fetched on
/// demand, which is the production scenario.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn queue_seek_long_cold_cache_far_segment(temp_dir: TestTempDir) {
    install_tracing();
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    // 20 segments × 4s each = 80s total. Seek at 40s lands in segment
    // 10, far past the head-of-line segments the initial play would
    // pre-fetch. With the 150ms per-segment delay the scheduler has
    // to actually drive the new fetch before the watchdog's 5s budget
    // expires.
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(20)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000])
        .packaged_audio_aac_lc(44_100, 2)
        .push_delay_rule(DelayRule {
            delay_ms: 150,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create long HLS fixture");
    let master = created.master_url();

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

    // Mirror the production pattern: one shared `Downloader` across
    // every track (see `kithara-app/src/sources.rs:21`). If the hang
    // is caused by shared HTTP pool / runtime contention, this is
    // where it would show up.
    let downloader = Downloader::new(DownloaderConfig::default());
    // `temp_dir` is the shared `temp_dir` fixture — auto-deleted when
    // the test returns. Keeps the shared app cache at
    // `env::temp_dir()/kithara` untouched.
    let store = StoreOptions::new(temp_dir.path());
    let track_source = |url: &str| -> TrackSource {
        let mut cfg = ResourceConfig::new(url)
            .expect("valid URL")
            .with_downloader(downloader.clone());
        cfg.store = store.clone();
        TrackSource::Config(Box::new(cfg))
    };

    let mut rx = queue.subscribe();
    let id = queue.append(track_source(master.as_str()));
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load: {e}"));
    queue.select(id, Transition::None).expect("select");
    queue.play();

    let pos_before = wait_for_position_at_least(&queue, 2.0, Duration::from_secs(30))
        .await
        .expect("track never played past 2s");
    eprintln!("[long-cold] pre-seek pos={pos_before:.3}s");

    let seek_target = 40.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[long-cold] seek issued target={seek_target:.1}s");

    let observation_deadline = Instant::now() + Duration::from_secs(20);
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
            Ok(()) => panic!("tick task exited unexpectedly"),
            Err(e) => panic!("seek watchdog panicked — hang reproduced: {e}"),
        }
    }

    assert!(
        confirmed,
        "long-cold seek to {seek_target:.2}s never advanced past target \
         (pos_before={pos_before:.2}, last={:?}) — hang without watchdog panic",
        queue.position_seconds(),
    );

    tick_handle.abort();
    drop(queue);
}

/// Multi-variant ABR variant: 3 variants × 30 segments × 4s = 120s each,
/// bandwidths spread wide (300k / 800k / 1.8M). The slow variant has a
/// 250ms per-segment delay so the ABR controller is tempted to switch
/// during warm-up playback. Then we seek far ahead (80s) into a cold
/// portion of the stream.
///
/// Reproduces the silvercomet-style production hang: `seek past EOF:
/// new_pos≈2GB len≈27MB seek_from=Current(≈2GB)` → decoder recreate
/// loop every ~17ms → seek watchdog panic after 5s.
///
/// Hypothesis: symphonia's isomp4 moof table accumulates fragment
/// offsets from multiple variants (each variant has its own
/// `tfhd.base_data_offset` space). After an ABR switch + seek, the
/// decoder's fragment table points at absolute offsets that exceed
/// the physical stream length, so `source.seek(Current(delta))` lands
/// past EOF forever. `align_decoder_with_seek_anchor` recreates the
/// decoder but the anchor path then fails again on the same mismatch.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn queue_seek_multi_variant_cold_far(temp_dir: TestTempDir) {
    install_tracing();
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let builder = HlsFixtureBuilder::new()
        .variant_count(3)
        .segments_per_variant(30)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_800_000, 800_000, 300_000])
        .packaged_audio_aac_lc(44_100, 2)
        // Slow the mid variant to nudge the ABR controller between
        // variants during the warm-up window.
        .push_delay_rule(DelayRule {
            variant: Some(1),
            delay_ms: 250,
            ..DelayRule::default()
        })
        // Every variant has a small baseline delay so cold-cache
        // fetches are not instantaneous — otherwise the seek may
        // complete before the watchdog can observe the hang.
        .push_delay_rule(DelayRule {
            delay_ms: 80,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create multi-variant HLS fixture");
    let master = created.master_url();

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
    // `temp_dir` is the shared `temp_dir` fixture — auto-deleted when
    // the test returns. Keeps the shared app cache at
    // `env::temp_dir()/kithara` untouched.
    let store = StoreOptions::new(temp_dir.path());
    let track_source = |url: &str| -> TrackSource {
        let mut cfg = ResourceConfig::new(url)
            .expect("valid URL")
            .with_downloader(downloader.clone());
        cfg.store = store.clone();
        TrackSource::Config(Box::new(cfg))
    };

    let mut rx = queue.subscribe();
    let id = queue.append(track_source(master.as_str()));
    wait_for_status(
        &mut rx,
        &queue,
        id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load: {e}"));
    queue.select(id, Transition::None).expect("select");
    queue.play();

    let pos_before = wait_for_position_at_least(&queue, 2.0, Duration::from_secs(30))
        .await
        .expect("track never played past 2s");
    eprintln!("[multi-variant-cold] pre-seek pos={pos_before:.3}s");

    // Target far ahead (80s of a 120s track). With cold cache + ABR
    // this is where silvercomet reproduces the hang in production.
    let seek_target = 80.0;
    queue.seek(seek_target).expect("seek accepted");
    eprintln!("[multi-variant-cold] seek issued target={seek_target:.1}s");

    let observation_deadline = Instant::now() + Duration::from_secs(25);
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
            Ok(()) => panic!("tick task exited unexpectedly"),
            Err(e) => panic!("seek watchdog panicked — hang reproduced: {e}"),
        }
    }

    assert!(
        confirmed,
        "multi-variant cold seek to {seek_target:.2}s never advanced past target \
         (pos_before={pos_before:.2}, last={:?}) — hang without watchdog panic",
        queue.position_seconds(),
    );

    tick_handle.abort();
    drop(queue);
}
