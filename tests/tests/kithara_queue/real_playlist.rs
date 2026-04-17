//! Real-network integration tests driving `Queue` against
//! `AppConfig::DEFAULT_TRACKS` through an offline audio backend.
//!
//! Both tests in this file hit the live silvercomet.top / zvq.me
//! endpoints and run the **exact production pipeline** (`Queue` →
//! `PlayerImpl` → `SessionState<OfflineBackend>` → firewheel). They are
//! marked `#[ignore]` because they require the network; run with
//! `--ignored --nocapture --test-threads=1`.
//!
//! DRM tracks currently fail on load (zvq.me stage server returns 403
//! "User not registered"). The tests document that regression instead
//! of hiding it — per-track smoke (`track_plays_end_to_end`) isolates
//! it to specific tracks, and the playlist scenario
//! (`queue_playlist_behavior`) surfaces it as a structured per-track
//! failure report.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    sync::{Arc, Once},
    time::Duration,
};

use kithara_app::{config::AppConfig, sources::build_source};
use kithara_events::{Event, EventReceiver, QueueEvent, TrackId, TrackStatus};
use kithara_net::NetOptions;
use kithara_queue::{Queue, QueueConfig, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{Xorshift64, kithara};
use tokio::{
    sync::OnceCell,
    time::{sleep, timeout},
};

// ─── shared test context ────────────────────────────────────────────

/// Per-process singleton: one offline audio session, one Downloader,
/// one Queue. `#[case]` tests inside this file share it, so init cost
/// (network TLS context, audio graph) is paid once.
struct TestCtx {
    config: AppConfig,
    queue: Arc<Queue>,
}

static TEST_CTX: OnceCell<TestCtx> = OnceCell::const_new();
static INIT_OFFLINE: Once = Once::new();

async fn shared_test_ctx() -> &'static TestCtx {
    TEST_CTX
        .get_or_init(|| async {
            // Claim the session singleton with OfflineBackend *before*
            // any PlayerImpl / Queue construction. Once.call_once
            // guarantees exactly one initialization per process.
            INIT_OFFLINE.call_once(kithara_play::internal::init_offline_backend);

            let net = NetOptions {
                insecure: true,
                ..NetOptions::default()
            };
            let downloader = Downloader::new(DownloaderConfig::default().with_net(net));
            let config = AppConfig::new(downloader);
            let queue = Arc::new(Queue::new(QueueConfig::default().with_autoplay(true)));

            // Background tick driver: Queue::tick updates cached
            // position, drains engine events, and arms crossfade. In
            // prod it's called from the UI loop; in tests we spawn a
            // tokio task that outlives the whole test binary.
            let queue_for_tick = Arc::clone(&queue);
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_millis(50)).await;
                    let _ = queue_for_tick.tick();
                }
            });

            TestCtx { config, queue }
        })
        .await
}

// ─── event/position helpers ─────────────────────────────────────────

async fn wait_for_status(
    rx: &mut EventReceiver,
    track_id: TrackId,
    target: TrackStatus,
    deadline: Duration,
) -> Result<(), String> {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    let res = timeout(deadline, async {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(RecvError::Lagged(_)) => continue, // skip dropped events on broadcast overflow
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
        Err(_) => Err(format!(
            "timeout waiting for {target:?} after {:?}",
            deadline
        )),
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
                "position stayed below {min_secs:.2}s for {:?} (last: {:?})",
                deadline,
                queue.position_seconds()
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
                "position never reached {target:.2}s (±{tolerance:.2}) in {:?} (last: {:?})",
                deadline,
                queue.position_seconds()
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

fn assert_monotonic_nondecreasing(samples: &[f64], url: &str) {
    for w in samples.windows(2) {
        assert!(
            w[1] >= w[0] - 0.05,
            "position regressed on [{url}]: {samples:?}"
        );
    }
}

// ─── per-track parametrized smoke ────────────────────────────────────

/// For each URL in the production playlist: load → play → seek ×3
/// random → position consistency. Isolates track-specific regressions
/// (DRM 403, MP3 seek-near-end hang, position drift).
#[kithara::test(tokio)]
#[ignore] // real network
#[case::silvercomet_mp3("https://stream.silvercomet.top/track.mp3", 42)]
#[case::silvercomet_hls("https://stream.silvercomet.top/hls/master.m3u8", 42)]
#[case::silvercomet_drm("https://stream.silvercomet.top/drm/master.m3u8", 42)]
#[case::zvuk_hq_1("https://cdn-edge.zvq.me/track/streamhq?id=27390231", 42)]
#[case::zvuk_hq_2("https://cdn-edge.zvq.me/track/streamhq?id=151585912", 42)]
#[case::zvuk_hq_3("https://cdn-edge.zvq.me/track/streamhq?id=125475417", 42)]
#[case::zvuk_drm_1(
    "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8",
    42
)]
#[case::zvuk_hls_1(
    "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
    42
)]
#[case::zvuk_drm_2(
    "https://ecs-stage-slicer-01.zvq.me/drm/track/176000094_1/master.m3u8",
    42
)]
#[case::zvuk_hls_2(
    "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
    42
)]
async fn track_plays_end_to_end(#[case] url: &str, #[case] rng_seed: u64) {
    let ctx = shared_test_ctx().await;
    let source = build_source(url, &ctx.config);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);

    // (a) Load
    wait_for_status(
        &mut rx,
        track_id,
        TrackStatus::Loaded,
        Duration::from_secs(30),
    )
    .await
    .unwrap_or_else(|e| panic!("load fail [{url}]: {e}"));

    // (b) Play + monotonic progress. Position must advance steadily,
    // never regress, after the decoder gets past its warm-up.
    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");
    wait_for_position_at_least(&ctx.queue, 0.5, Duration::from_secs(15))
        .await
        .unwrap_or_else(|e| panic!("play fail [{url}]: {e}"));
    let progress = sample_positions(&ctx.queue, 5, Duration::from_millis(200)).await;
    assert_monotonic_nondecreasing(&progress, url);

    // (c) Seed-deterministic seek × 3 random
    let duration = ctx
        .queue
        .duration_seconds()
        .expect("duration known after Loaded");
    let mut rng = Xorshift64::new(rng_seed);
    for i in 0..3 {
        let target = duration * rng.range_f64(0.05, 0.95);
        ctx.queue.seek(target).expect("seek");
        wait_for_position_near(&ctx.queue, target, 1.0, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("seek #{i} to {target:.1}s fail [{url}]: {e}"));
        // Hang detection: decoder must advance by ≥1s over next 2s.
        // OfflineBackend runs at ~70% realtime so 1s is a comfortable
        // floor below real advance (~1.4s expected) but above any
        // decoder stall.
        let before = ctx.queue.position_seconds().unwrap_or(0.0);
        sleep(Duration::from_secs(2)).await;
        let after = ctx.queue.position_seconds().unwrap_or(0.0);
        assert!(
            after - before >= 1.0,
            "seek #{i} hang [{url}]: {before:.2}→{after:.2} over 2s"
        );
    }

    // (d) Position consistency — 2s wall clock should produce ~1.4s
    // audio position advance on OfflineBackend (~70% realtime). Any
    // drift beyond ±0.5s of that window indicates a real bug in
    // position reporting (slider-ahead, PTS reset, or similar).
    let start_pos = ctx.queue.position_seconds().unwrap_or(0.0);
    sleep(Duration::from_secs(2)).await;
    let end_pos = ctx.queue.position_seconds().unwrap_or(0.0);
    let gain = end_pos - start_pos;
    assert!(
        (0.9..=1.9).contains(&gain),
        "position gain out of offline-realtime window [{url}]: got \
         {gain:.2}s over 2s wall clock (expected 0.9..1.9; start=\
         {start_pos:.2} end={end_pos:.2})",
    );

    ctx.queue.remove(track_id).expect("remove");
}
