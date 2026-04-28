//! Pin: **once the user picks track B while track A is still loading,
//! track A must never become the playing track**.
//!
//! Reproduces the user-reported "barge-in" bug from `kithara-app`:
//! "I click track1, nothing happens. I click another track2, that one
//! plays. After a short pause, track1 barges in." The architectural
//! cause sits in `Queue::spawn_apply_after_load` (queue.rs:700):
//! `replace_item(index, resource)` runs **unconditionally** for every
//! finished load, planting the slow track's resource in its queue slot
//! even when the user has already moved on. The next auto-advance then
//! plays that slot, surprising the user.
//!
//! Layout:
//! - `fast` HLS fixture, **1 segment × 2 s**, low per-segment delay.
//! - `slow` HLS fixture, longer (≥ 12 s), high per-segment delay so its
//!   loader finishes after `fast` is already playing.
//! - Append `fast` first (index 0), `slow` second (index 1). Picking
//!   `fast` lands the player on index 0; when `fast` ends naturally,
//!   AVQueuePlayer-style auto-advance will move to index 1 (`slow`).
//!
//! Sequence per iteration:
//! 1. `select(slow_id, Transition::None)` — slow goes Pending,
//!    `pending_select := Some(slow)`, loader spawned.
//! 2. ~50 ms gap so the slow loader is genuinely Loading.
//! 3. `select(fast_id, Transition::None)` — `pending_select` overwritten
//!    to `Some(fast)`. Both loaders now run in parallel.
//! 4. Wait for both to settle. `fast` lands as current and plays.
//! 5. Wait long enough for `fast` to end naturally (~3 s).
//! 6. **Bug surfaces** as `current()` flipping to `slow_id` via
//!    auto-advance even though the user never asked to play slow.
//!
//! Stress: parametrised over `iterations` so a 30-pass run pins the
//! 1/N flake user reports in `kithara-app`.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    sync::{Arc, Once},
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, EventReceiver, TrackId, TrackStatus};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::DelayRule, kithara,
    temp_dir,
};
use tokio::time::sleep;
use url::Url;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    /// Number of repetitions inside one parametrised case. The user
    /// reports a 1/19 flake; 30 iterations gives ≈80 % catch.
    const STRESS_ITERATIONS: u32 = 30;
    /// Per-segment delay applied to the *slow* fixture. Big enough that
    /// its loader is still running when the second `select()` fires,
    /// small enough that `Resource::new()` does not give up during
    /// init/sidx fetches.
    const SLOW_DELAY_MS: u64 = 200;
    /// Per-segment delay on the *fast* fixture — small.
    const FAST_DELAY_MS: u64 = 30;
    /// Wall-time gap between `select(slow)` and `select(fast)`. Long
    /// enough for slow to reach `Loading`, short enough that it has not
    /// finished before the second select stomps `pending_select`.
    const SELECT_GAP_MS: u64 = 50;
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(30);
    /// `fast` fixture duration: 1 segment × 2 s = 2 s. Test must wait
    /// at least this long after the fast track starts playing for the
    /// natural end → auto-advance window to open.
    const FAST_SEGMENT_COUNT: usize = 1;
    const FAST_SEGMENT_DURATION_S: f64 = 2.0;
    /// `slow` fixture: enough segments that it cannot finish during
    /// the observation window even at low delay.
    const SLOW_SEGMENT_COUNT: usize = 4;
    const SLOW_SEGMENT_DURATION_S: f64 = 4.0;
    /// Window after `fast` starts playing during which the test watches
    /// `current()` for an unauthorised flip to `slow_id`. Must exceed
    /// `FAST_SEGMENT_DURATION_S` so the natural end fires inside the
    /// window and auto-advance can attempt to play slow.
    const POST_FAST_OBSERVE: Duration = Duration::from_secs(5);
    /// Polling interval for `current()` during the watch.
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
}

async fn build_hls(
    helper: &TestServerHelper,
    delay_ms: u64,
    segment_count: usize,
    segment_duration_s: f64,
) -> Url {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(segment_count)
        .segment_duration_secs(segment_duration_s)
        .packaged_audio_aac_lc(44_100, 2);
    if delay_ms > 0 {
        builder = builder.delay_rules(vec![DelayRule {
            variant: None,
            segment_eq: None,
            segment_gte: None,
            delay_ms,
        }]);
    }
    helper
        .create_hls(builder)
        .await
        .expect("create local HLS fixture")
        .master_url()
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

async fn wait_for_loader_done(
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(entry) = queue.track(track_id) {
            match &entry.status {
                TrackStatus::Loaded | TrackStatus::Consumed | TrackStatus::Cancelled => {
                    return Ok(());
                }
                TrackStatus::Failed(err) => return Err(format!("track entered Failed: {err}")),
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

async fn wait_for_current_id(
    queue: &Queue,
    expected: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if queue.current().map(|e| e.id) == Some(expected) {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "current never became {:?} within {deadline:?} (last={:?})",
                expected,
                queue.current().map(|e| e.id),
            ));
        }
        sleep(Duration::from_millis(40)).await;
    }
}

fn drain_event_backlog(rx: &mut EventReceiver) {
    while rx.try_recv().is_ok() {}
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::single_pass(1)]
#[case::stress_pass(Consts::STRESS_ITERATIONS)]
async fn track_switch_race_does_not_let_slow_track_barge_in(#[case] iterations: u32) {
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let fast_url = build_hls(
        &helper,
        Consts::FAST_DELAY_MS,
        Consts::FAST_SEGMENT_COUNT,
        Consts::FAST_SEGMENT_DURATION_S,
    )
    .await;
    let slow_url = build_hls(
        &helper,
        Consts::SLOW_DELAY_MS,
        Consts::SLOW_SEGMENT_COUNT,
        Consts::SLOW_SEGMENT_DURATION_S,
    )
    .await;

    let mut barge_ins: Vec<String> = Vec::new();

    for iter in 0..iterations {
        let temp = temp_dir();
        let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

        let mk_cfg = |url: &Url| {
            let mut cfg = ResourceConfig::new(url.as_str()).expect("valid fixture URL");
            cfg = cfg.with_downloader(downloader.clone());
            cfg.store = store.clone();
            cfg.initial_abr_mode = AbrMode::Auto(None);
            cfg
        };

        // fast first (index 0), slow second (index 1) — auto-advance
        // after fast natural end will reach slow's slot.
        let fast_id = queue.append(TrackSource::Config(Box::new(mk_cfg(&fast_url))));
        let slow_id = queue.append(TrackSource::Config(Box::new(mk_cfg(&slow_url))));

        let mut rx = queue.subscribe();
        drain_event_backlog(&mut rx);

        // Race window: pending_select goes slow then is stomped by fast.
        queue
            .select(slow_id, Transition::None)
            .expect("select slow");
        sleep(Duration::from_millis(Consts::SELECT_GAP_MS)).await;
        queue
            .select(fast_id, Transition::None)
            .expect("select fast");

        wait_for_loader_done(&queue, fast_id, Consts::LOAD_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] fast load: {e}"));
        wait_for_loader_done(&queue, slow_id, Consts::LOAD_DEADLINE)
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] slow load: {e}"));

        // fast must actually become current and start playing.
        wait_for_current_id(&queue, fast_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("[iter {iter}] fast never became current: {e}"));

        // Watch the post-fast window: the bug is current flipping to
        // slow via auto-advance after fast ends naturally.
        let watch_start = std::time::Instant::now();
        let mut current_history: Vec<Option<TrackId>> = Vec::new();
        let mut barged = false;
        while watch_start.elapsed() < Consts::POST_FAST_OBSERVE {
            let cur = queue.current().map(|e| e.id);
            if cur != current_history.last().copied().flatten().and_then(Some) {
                current_history.push(cur);
            }
            if cur == Some(slow_id) {
                barge_ins.push(format!(
                    "[iter {iter}] auto-advance played slow_id after {elapsed_ms} ms \
                     (history={current_history:?})",
                    elapsed_ms = watch_start.elapsed().as_millis(),
                ));
                barged = true;
                break;
            }
            sleep(Consts::POLL_INTERVAL).await;
        }

        // If we did not flip to slow, there should be at most a transition
        // fast → None (QueueEnded) — not fast → slow.
        if !barged {
            // Soft sanity: at least one of fast→None or fast persisting.
            // (Diagnostic; not asserted as failure on its own.)
        }

        tick_handle.abort();
    }

    if !barge_ins.is_empty() {
        panic!(
            "track_switch_race: {n}/{iterations} iteration(s) saw slow_id barge in:\n{}",
            barge_ins.join("\n"),
            n = barge_ins.len(),
        );
    }
}
