//! Reproducer for the user-reported "seek to end of an HLS track hangs,
//! then logs `seek failed`" bug from `kithara-app`.
//!
//! User scenario verbatim: "просто открыл плеер и после этого выбрал hls
//! и перемотал почти в конец трека". So each iteration constructs a
//! **fresh** Queue + Player + Track and issues a single near-end seek
//! after a minimal warmup — there is no carry-over decoder/timeline
//! state from a previous run.
//!
//! What we pin:
//! - The seek lands at `target ± 1 s` (offline pacing tolerance) and
//!   playback advances at least `MIN_POST_SEEK_ADVANCE_S` afterwards,
//!   OR
//! - The seek raises a hard typed error within `SEEK_BUDGET` (the bug
//!   user reports is a >10 s hang followed by a delayed `seek failed`,
//!   so a fast typed error is preferable but still tracked).
//!
//! Failures are bucketed (hang vs error) and reported with the
//! iteration index so the trace log on disk can be inspected.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    sync::{Arc, Once},
    time::Duration,
};

use kithara_assets::StoreOptions;
use kithara_events::{AbrMode, TrackId, TrackStatus};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig, internal::init_offline_backend};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::{HlsFixtureBuilder, TestServerHelper, TestTempDir, kithara, temp_dir};
use tokio::time::sleep;
use url::Url;

static INIT_OFFLINE: Once = Once::new();

struct Consts;
impl Consts {
    /// Number of fresh-player iterations. User reports the bug at
    /// roughly 1/19; 30 fresh runs gives >80 % catch probability if
    /// the production rate is comparable.
    const FRESH_ITERATIONS: u32 = 30;
    /// HLS fixture shape: 8 segments × 4 s = 32 s total. Big enough
    /// that "near end" is well past the warmup window and any byte-range
    /// cache the loader populated for early segments.
    const SEGMENT_COUNT: usize = 8;
    const SEGMENT_DURATION_S: f64 = 4.0;
    /// Distance from the natural end at which the seek targets land.
    /// Rotated across iterations so we hit boundary-aligned and
    /// mid-segment ends.
    const NEAR_END_OFFSETS_S: [f64; 3] = [0.5, 1.5, 3.5];
    /// Each individual seek must land or fail within this budget.
    /// A hang is anything > this (the user reports >10 s freezes).
    const SEEK_BUDGET: Duration = Duration::from_secs(6);
    /// Minimum post-seek position advance we require to declare a
    /// landed seek as "playing again".
    const MIN_POST_SEEK_ADVANCE_S: f64 = 0.5;
    /// Time given to the player to consume some PCM after the seek
    /// before we sample position.
    const POST_SEEK_RENDER_WALL: Duration = Duration::from_millis(1_500);
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(30);
    /// Pre-seek warmup. Mirrors "just opened, briefly listened, then
    /// dragged the playhead". Short enough that the player is still
    /// inside segment 0 when seek fires — prod scenario from the user.
    const PRE_SEEK_PLAY_S: f64 = 0.5;
}

#[derive(Debug)]
enum IterOutcome {
    Ok,
    Hung {
        iter: u32,
        target: f64,
        pos_before: f64,
        pos_after: f64,
        budget_ms: u128,
    },
    Errored {
        iter: u32,
        target: f64,
        error: String,
    },
}

async fn build_hls(helper: &TestServerHelper) -> Url {
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(Consts::SEGMENT_COUNT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2);
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
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
                TrackStatus::Loaded | TrackStatus::Consumed => return Ok(()),
                TrackStatus::Failed(err) => return Err(format!("loader failed: {err}")),
                _ => {}
            }
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "loader timeout {deadline:?} (last={:?})",
                queue.track(track_id).map(|e| e.status)
            ));
        }
        sleep(Duration::from_millis(40)).await;
    }
}

async fn wait_for_position_at_least(
    queue: &Queue,
    min: f64,
    deadline: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Some(p) = queue.position_seconds()
            && p >= min
        {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "position never reached {min:.2}s in {deadline:?} (last={:?})",
                queue.position_seconds()
            ));
        }
        sleep(Duration::from_millis(40)).await;
    }
}

/// Run one fresh-player attempt: spin up Queue + Player, append the HLS
/// track, wait for loader, briefly play, seek to `target`, observe.
///
/// Returns `IterOutcome::Ok` if the seek landed within tolerance and
/// playback advanced afterwards; otherwise `Hung` or `Errored`.
async fn run_one_attempt(iter: u32, url: &Url, target_offset: f64) -> IterOutcome {
    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let mut cfg = match ResourceConfig::new(url.as_str()) {
        Ok(cfg) => cfg,
        Err(e) => {
            tick_handle.abort();
            return IterOutcome::Errored {
                iter,
                target: f64::NAN,
                error: format!("ResourceConfig::new failed: {e}"),
            };
        }
    };
    cfg = cfg.with_downloader(downloader.clone());
    cfg.store = store;
    cfg.initial_abr_mode = AbrMode::Auto(None);
    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));

    if let Err(e) = queue.select(track_id, Transition::None) {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("queue.select failed: {e}"),
        };
    }

    if let Err(e) = wait_for_loader_done(&queue, track_id, Consts::LOAD_DEADLINE).await {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("loader: {e}"),
        };
    }

    if let Err(e) =
        wait_for_position_at_least(&queue, Consts::PRE_SEEK_PLAY_S, Duration::from_secs(15)).await
    {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("warmup: {e}"),
        };
    }

    let duration = match queue.duration_seconds() {
        Some(d) => d,
        None => {
            tick_handle.abort();
            return IterOutcome::Errored {
                iter,
                target: f64::NAN,
                error: "duration unknown after Loaded".into(),
            };
        }
    };

    let target = (duration - target_offset).max(0.0);
    let pos_before = queue.position_seconds().unwrap_or(0.0);

    let seek_started = std::time::Instant::now();
    if let Err(e) = queue.seek(target) {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target,
            error: format!("queue.seek returned Err: {e}"),
        };
    }

    let mut landed = false;
    let mut errored: Option<String> = None;
    while seek_started.elapsed() < Consts::SEEK_BUDGET {
        if let Some(entry) = queue.track(track_id)
            && let TrackStatus::Failed(err) = &entry.status
        {
            errored = Some(format!("track entered Failed: {err}"));
            break;
        }
        if let Some(p) = queue.position_seconds()
            && (p - target).abs() < 1.0
        {
            landed = true;
            break;
        }
        sleep(Duration::from_millis(30)).await;
    }

    if let Some(error) = errored {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target,
            error,
        };
    }
    if !landed {
        let pos_after = queue.position_seconds().unwrap_or(0.0);
        tick_handle.abort();
        return IterOutcome::Hung {
            iter,
            target,
            pos_before,
            pos_after,
            budget_ms: Consts::SEEK_BUDGET.as_millis(),
        };
    }

    sleep(Consts::POST_SEEK_RENDER_WALL).await;
    let pos_after = queue.position_seconds().unwrap_or(0.0);
    tick_handle.abort();
    let advance = pos_after - target;
    if advance < Consts::MIN_POST_SEEK_ADVANCE_S {
        IterOutcome::Hung {
            iter,
            target,
            pos_before,
            pos_after,
            budget_ms: Consts::SEEK_BUDGET.as_millis(),
        }
    } else {
        IterOutcome::Ok
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(900)))]
async fn hls_seek_near_end_fresh_player_stress() {
    INIT_OFFLINE.call_once(init_offline_backend);

    let helper = TestServerHelper::new().await;
    let url = build_hls(&helper).await;

    let mut outcomes: Vec<IterOutcome> = Vec::with_capacity(Consts::FRESH_ITERATIONS as usize);
    for iter in 0..Consts::FRESH_ITERATIONS {
        let offset = Consts::NEAR_END_OFFSETS_S[(iter as usize) % Consts::NEAR_END_OFFSETS_S.len()];
        let outcome = run_one_attempt(iter, &url, offset).await;
        outcomes.push(outcome);
    }

    let mut hangs: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for o in &outcomes {
        match o {
            IterOutcome::Ok => {}
            IterOutcome::Hung {
                iter,
                target,
                pos_before,
                pos_after,
                budget_ms,
            } => hangs.push(format!(
                "[iter {iter}] hang: target={target:.2}s pos_before={pos_before:.2}s \
                 pos_after={pos_after:.2}s budget={budget_ms}ms"
            )),
            IterOutcome::Errored {
                iter,
                target,
                error,
            } => errors.push(format!("[iter {iter}] error: target={target:.2}s {error}")),
        }
    }

    if !hangs.is_empty() || !errors.is_empty() {
        panic!(
            "hls_seek_near_end_fresh_player_stress: {n_hung} hang(s), {n_err} error(s) over \
             {n} fresh-player iterations:\nHangs:\n{hangs}\nErrors:\n{errors}",
            n_hung = hangs.len(),
            n_err = errors.len(),
            n = Consts::FRESH_ITERATIONS,
            hangs = hangs.join("\n"),
            errors = errors.join("\n"),
        );
    }
}
