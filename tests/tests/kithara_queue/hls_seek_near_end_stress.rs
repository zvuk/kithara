#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    assets::AssetStore,
    decode::DecoderBackend,
    events::{AbrMode, AudioEvent, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{self, Duration, timeout},
        tokio,
        tokio::sync::broadcast::error::RecvError,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, kithara,
    offline::OfflineSession,
    temp_dir,
    waits::{wait_for_loader_done_event, wait_for_position_event},
};
use url::Url;

struct Consts;
impl Consts {
    /// Number of fresh-player iterations. User reports the bug at
    /// roughly 1/19; 30 fresh runs gives >80 % catch probability if
    /// the production rate is comparable. Bounded by `IterDeadline`
    /// below so a hung iteration cannot exceed its share of the
    /// outer test timeout.
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
    ///
    /// Decision log: the AAC decoder strips its algorithmic delay
    /// (`outputDelay`, ~40 ms for AAC-LC @ 44.1 kHz) at the head of the
    /// PCM stream — the visible track length is correspondingly
    /// shorter than the container duration. The closest seek offset
    /// in [`Self::NEAR_END_OFFSETS_S`] is 0.5 s; with the strip eating
    /// ~0.04 s of that residual budget, the largest reliable post-seek
    /// advance for that offset is ~0.45 s. Using 0.4 leaves ~0.05 s of
    /// margin so the iter does not spuriously hang at the EOF.
    const MIN_POST_SEEK_ADVANCE_S: f64 = 0.4;
    /// Time given to the player to consume some PCM after the seek
    /// before we sample position.
    const POST_SEEK_RENDER_WALL: Duration = Duration::from_millis(1_500);
    /// Loader settle deadline.
    const LOAD_DEADLINE: Duration = Duration::from_secs(15);
    /// Pre-seek warmup. Mirrors "just opened, briefly listened, then
    /// dragged the playhead". Short enough that the player is still
    /// inside segment 0 when seek fires — prod scenario from the user.
    const PRE_SEEK_PLAY_S: f64 = 0.5;
    /// Hard ceiling per iteration. Anything past this is a hang in
    /// `run_one_attempt` itself (e.g. the player never produces
    /// position updates). Bounds the worst-case test runtime to
    /// `FRESH_ITERATIONS * ITER_DEADLINE` ≈ 5 min.
    const ITER_DEADLINE: Duration = Duration::from_secs(10);
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

async fn build_hls(helper: &TestServerHelper, include_sidx: bool) -> Url {
    let builder = HlsFixtureBuilder::new()
        .variant_count(1)
        .segments_per_variant(Consts::SEGMENT_COUNT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2)
        .include_sidx(include_sidx);
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
        .master_url()
}

/// Queue tick driver, flash-coherent: spawned through the platform chokepoint
/// (participant + ambient propagation) with `#[kithara::flash(true)]` so the
/// 50ms cadence rides the VIRTUAL clock under flash. The previous raw
/// `tokio::spawn` + bare real sleep ran invisible to the engine: the test's
/// virtual `ITER_DEADLINE` elapsed after a handful of real ticks under load.
#[kithara::flash(true)]
async fn drive_queue_ticks(queue: Arc<Queue>) {
    loop {
        time::sleep(Duration::from_millis(50)).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Downloader,
    AssetStore,
    tokio::task::JoinHandle<()>,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(
        QueueConfig::default()
            .with_player(player)
            .with_store(store.clone()),
    ));
    let tick_handle = tokio::task::spawn(drive_queue_ticks(Arc::clone(&queue)));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    (queue, downloader, store, tick_handle)
}

/// Run one fresh-player attempt: spin up Queue + Player, append the HLS
/// track, wait for loader, briefly play, seek to `target`, observe.
///
/// Returns `IterOutcome::Ok` if the seek landed within tolerance and
/// playback advanced afterwards; otherwise `Hung` or `Errored`.
async fn run_one_attempt(
    iter: u32,
    url: &Url,
    target_offset: f64,
    backend: DecoderBackend,
) -> IterOutcome {
    let temp = temp_dir();
    let (queue, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let builder = match ResourceConfig::for_src(url.as_str()) {
        Ok(b) => b,
        Err(e) => {
            tick_handle.abort();
            return IterOutcome::Errored {
                iter,
                target: f64::NAN,
                error: format!("ResourceConfig::for_src failed: {e}"),
            };
        }
    };
    let cfg = builder
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(store)
        .initial_abr_mode(AbrMode::Auto(None))
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .build();
    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));

    // Subscribe before the action that drives loading/playback so no
    // status or progress event can slip between `select` and the first
    // `recv`. The queue bus is the player bus (`Queue::new` clones
    // `player.bus()`), so audio sink-truth events arrive here too.
    let mut rx = queue.subscribe();

    if let Err(e) = queue.select(track_id, Transition::None) {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("queue.select failed: {e}"),
        };
    }

    if let Err(e) =
        wait_for_loader_done_event(&mut rx, &queue, track_id, Consts::LOAD_DEADLINE).await
    {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("loader: {e}"),
        };
    }

    if let Err(e) = wait_for_position_event(
        &mut rx,
        &queue,
        Consts::PRE_SEEK_PLAY_S,
        Duration::from_secs(15),
    )
    .await
    {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: format!("warmup: {e}"),
        };
    }

    let duration = if let Some(d) = queue.duration_seconds() {
        d
    } else {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target: f64::NAN,
            error: "duration unknown after Loaded".into(),
        };
    };

    let target = (duration - target_offset).max(0.0);
    let pos_before = queue.position_seconds().unwrap_or(0.0);

    if let Err(e) = queue.seek(target) {
        tick_handle.abort();
        return IterOutcome::Errored {
            iter,
            target,
            error: format!("queue.seek returned Err: {e}"),
        };
    }

    // Seek-landed: wait on the sink-truth events the worker emits after
    // applying the seek — `SeekComplete` (post-seek output committed) or a
    // `PlaybackProgress` whose position is within tolerance of `target` —
    // rather than polling the tick-cached position. A `Failed` transition is
    // the failure branch. The budget is a virtual hang ceiling under flash.
    match wait_for_seek_landed(&mut rx, &queue, track_id, target, Consts::SEEK_BUDGET).await {
        SeekLanded::Landed => {}
        SeekLanded::Failed(err) => {
            tick_handle.abort();
            return IterOutcome::Errored {
                iter,
                target,
                error: err,
            };
        }
        SeekLanded::Timeout => {
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
    }

    // Post-seek "playing again": wait for real PCM-commit progress past the
    // seek target by `MIN_POST_SEEK_ADVANCE_S`, observed via
    // `PlaybackProgress`, rather than a fixed render wall.
    match wait_for_post_seek_advance(
        &mut rx,
        &queue,
        track_id,
        target,
        Consts::MIN_POST_SEEK_ADVANCE_S,
        Consts::POST_SEEK_RENDER_WALL,
    )
    .await
    {
        PostSeekAdvance::Advanced => {
            tick_handle.abort();
            IterOutcome::Ok
        }
        PostSeekAdvance::Failed(err) => {
            tick_handle.abort();
            IterOutcome::Errored {
                iter,
                target,
                error: err,
            }
        }
        PostSeekAdvance::Timeout(pos_after) => {
            tick_handle.abort();
            IterOutcome::Hung {
                iter,
                target,
                pos_before,
                pos_after,
                budget_ms: Consts::SEEK_BUDGET.as_millis(),
            }
        }
    }
}

enum SeekLanded {
    Landed,
    Failed(String),
    Timeout,
}

/// Wait until the seek lands: a `SeekComplete` whose committed position is
/// within tolerance of `target`, or a `PlaybackProgress` position within
/// tolerance. `Failed` is surfaced; the budget caps the wait.
async fn wait_for_seek_landed(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    target: f64,
    budget: Duration,
) -> SeekLanded {
    if let Some(entry) = queue.track(track_id)
        && let TrackStatus::Failed(err) = &entry.status
    {
        return SeekLanded::Failed(format!("track entered Failed: {err}"));
    }
    let landed = timeout(budget, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    if ((position_ms as f64 / 1000.0) - target).abs() < 1.0 {
                        return Ok(());
                    }
                }
                Ok(Event::Audio(AudioEvent::SeekComplete { position, .. })) => {
                    if (position.as_secs_f64() - target).abs() < 1.0 {
                        return Ok(());
                    }
                }
                Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id,
                    status: TrackStatus::Failed(err),
                })) if id == track_id => {
                    return Err(format!("track entered Failed: {err}"));
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(entry) = queue.track(track_id)
                        && let TrackStatus::Failed(err) = &entry.status
                    {
                        return Err(format!("track entered Failed: {err}"));
                    }
                    if let Some(p) = queue.position_seconds()
                        && (p - target).abs() < 1.0
                    {
                        return Ok(());
                    }
                }
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await;
    match landed {
        Ok(Ok(())) => SeekLanded::Landed,
        Ok(Err(err)) => SeekLanded::Failed(err),
        Err(_) => SeekLanded::Timeout,
    }
}

enum PostSeekAdvance {
    Advanced,
    Failed(String),
    Timeout(f64),
}

/// Wait until playback advances at least `min_advance` seconds past
/// `target`, observed via `PlaybackProgress`. Proves "playing again after
/// seek" on real PCM-commit progress. `Failed` is surfaced; the budget caps
/// the wait and the timeout carries the last observed position.
async fn wait_for_post_seek_advance(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    target: f64,
    min_advance: f64,
    budget: Duration,
) -> PostSeekAdvance {
    if let Some(entry) = queue.track(track_id)
        && let TrackStatus::Failed(err) = &entry.status
    {
        return PostSeekAdvance::Failed(format!("track entered Failed (post-seek): {err}"));
    }
    if let Some(p) = queue.position_seconds()
        && (p - target) >= min_advance
    {
        return PostSeekAdvance::Advanced;
    }
    let advanced = timeout(budget, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    let p = position_ms as f64 / 1000.0;
                    if (p - target) >= min_advance {
                        return Ok(());
                    }
                }
                Ok(Event::Queue(QueueEvent::TrackStatusChanged {
                    id,
                    status: TrackStatus::Failed(err),
                })) if id == track_id => {
                    return Err(format!("track entered Failed (post-seek): {err}"));
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(entry) = queue.track(track_id)
                        && let TrackStatus::Failed(err) = &entry.status
                    {
                        return Err(format!("track entered Failed (post-seek): {err}"));
                    }
                    if let Some(p) = queue.position_seconds()
                        && (p - target) >= min_advance
                    {
                        return Ok(());
                    }
                }
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            }
        }
    })
    .await;
    match advanced {
        Ok(Ok(())) => PostSeekAdvance::Advanced,
        Ok(Err(err)) => PostSeekAdvance::Failed(err),
        Err(_) => PostSeekAdvance::Timeout(queue.position_seconds().unwrap_or(0.0)),
    }
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::symphonia_no_sidx(DecoderBackend::Symphonia, false)]
#[case::symphonia_with_sidx(DecoderBackend::Symphonia, true)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_no_sidx(DecoderBackend::Apple, false)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_with_sidx(DecoderBackend::Apple, true)
)]
#[cfg_attr(
    target_os = "android",
    case::android_no_sidx(DecoderBackend::Android, false)
)]
#[cfg_attr(
    target_os = "android",
    case::android_with_sidx(DecoderBackend::Android, true)
)]
async fn hls_seek_near_end_fresh_player_stress(
    #[case] backend: DecoderBackend,
    #[case] include_sidx: bool,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let helper = TestServerHelper::new().await;
    let url = build_hls(&helper, include_sidx).await;

    let mut outcomes: Vec<IterOutcome> = Vec::with_capacity(Consts::FRESH_ITERATIONS as usize);
    for iter in 0..Consts::FRESH_ITERATIONS {
        let offset = Consts::NEAR_END_OFFSETS_S[(iter as usize) % Consts::NEAR_END_OFFSETS_S.len()];
        let outcome = match time::timeout(
            Consts::ITER_DEADLINE,
            run_one_attempt(iter, &url, offset, backend),
        )
        .await
        {
            Ok(o) => o,
            Err(_) => IterOutcome::Errored {
                iter,
                target: f64::NAN,
                error: format!(
                    "iteration exceeded ITER_DEADLINE ({:?}) — hung outside seek budget",
                    Consts::ITER_DEADLINE,
                ),
            },
        };
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
