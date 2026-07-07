#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use kithara::{
    assets::{FlushHub, FlushPolicy, StoreOptions},
    decode::DecoderBackend,
    events::{AbrMode, Event, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, Instant, sleep, timeout},
        tokio,
    },
    play::{PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::config::AppConfig;
use kithara_integration_tests::{TestTempDir, kithara, offline::OfflineSession};
use kithara_test_utils::probe::capture::{Recorder, install as install_recorder};

/// Captured from `app.log @ 06:39:13`. `cdn-hls-slicer.zvuk.com` →
/// `zvuk-prod` provider in baked `app.yaml`.
const TARGET_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/171515249_1/master.m3u8";

/// Absolute scrub target captured from `app.log` (the slider was at
/// ~45% of the 276.85s track). Fixed in seconds rather than as a
/// ratio because `Queue::duration_seconds()` is only populated after
/// the player has decoded at least one frame, and we want the scrub
/// to land outside the buffered range — the bug precondition.
const SCRUB_TARGET_SECS: f64 = 124.58;

/// Safety budgets. These are NOT timer-driven control flow — every
/// state transition is probe / event driven. Budgets fail the test
/// loudly rather than mask a hang. Chosen larger than realistic prod
/// latency (cold CDN cache, slow CI runner) so flakes surface as
/// real bugs, not flake margin.
const LOAD_BUDGET: Duration = Duration::from_secs(45);
const WARMUP_PROBE_BUDGET: Duration = Duration::from_secs(30);
const OUTCOME_BUDGET: Duration = Duration::from_secs(45);

/// Minimum playback growth past `SCRUB_TARGET_SECS` that proves the
/// engine actually resumed decoding after the seek (i.e. the user
/// hears audio, not silence). Smaller than the budget window so a
/// healthy run finishes fast; large enough that a single decoded
/// chunk's timestamp can't accidentally satisfy it.
const MIN_POST_SEEK_GROWTH_SECS: f64 = 1.0;

struct Ctx {
    config: AppConfig,
    queue: Arc<Queue>,
    cache: TestTempDir,
}

async fn build_ctx() -> Ctx {
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancelToken::never())).build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let config = AppConfig::new(downloader, flush_hub, CancelToken::never());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    let q = Arc::clone(&queue);
    tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            let _ = q.tick();
        }
    });

    Ctx {
        config,
        queue,
        cache: TestTempDir::new(),
    }
}

fn build_track_source(url: &str, ctx: &Ctx, backend: DecoderBackend) -> TrackSource {
    super::app_track_source(
        url,
        &ctx.config,
        StoreOptions::new(ctx.cache.path()),
        backend,
        AbrMode::Auto(None),
        None,
    )
}

async fn wait_for_loaded(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    if let Some(entry) = queue.track(track_id) {
        match &entry.status {
            TrackStatus::Loaded => return Ok(()),
            TrackStatus::Failed(err) => return Err(format!("Failed before subscribe: {err}")),
            _ => {}
        }
    }
    timeout(deadline, async {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return Err("event stream closed".to_string()),
            };
            if let Event::Queue(QueueEvent::TrackStatusChanged { id, status }) = ev
                && id == track_id
            {
                match status {
                    TrackStatus::Loaded => return Ok(()),
                    TrackStatus::Failed(err) => return Err(format!("Failed: {err}")),
                    _ => continue,
                }
            }
        }
    })
    .await
    .map_err(|_| format!("no Loaded within {deadline:?}"))?
}

/// Wait until the decoder has produced at least one PCM chunk for the
/// active track. Drives off the `kithara::decode::composed::build_chunk`
/// probe — fires the moment the demuxer + frame codec hand a chunk to
/// the audio pipeline, which is the production-truth signal for
/// "warmup complete". Replaces a wall-clock `wait_for_position`.
async fn wait_for_warmup(recorder: &Recorder, budget: Duration) -> Result<(), String> {
    let evt = recorder
        .wait_for_probe_async(
            |e| e.target == "kithara_decode_probe" && e.probe_name() == Some("build_chunk"),
            budget,
        )
        .await;
    if evt.is_none() {
        return Err(format!(
            "no `kithara_decode_probe::build_chunk` within {budget:?} — decoder produced no PCM"
        ));
    }
    Ok(())
}

/// Count `build_chunk` probe events emitted so far — the number of
/// PCM chunks the decoder has actually produced. The bug shape we
/// guard against post-seek: a stalled pipeline where the queue stays
/// on the target track and `position_seconds()` may even tick (timeline
/// can advance from buffer drain alone) but the **decoder is silent**
/// because the reader is stuck on garbage bytes or the codec is wedged.
/// Counting probe events is the production-truth check: if this number
/// doesn't grow after the seek, no new audio is being decoded — the
/// user hears nothing, regardless of what `position_seconds` reports.
fn count_build_chunks(recorder: &Recorder) -> usize {
    recorder.events_with_probe("build_chunk").len()
}

/// One observed event with a wall-clock relative timestamp. Captured
/// independently of `src` filtering — auto-advance produces events
/// for the *next* track, and the test needs to log those just as
/// loudly as events for the scrubbed track.
#[derive(Debug, Clone)]
struct TimedEvent {
    elapsed: Duration,
    event: Event,
}

/// What the engine did with the queue during the observation window.
///
/// `PlaybackContinued` is the *only* acceptable outcome: queue stayed
/// on the target track AND the position advanced past
/// `SCRUB_TARGET_SECS + MIN_POST_SEEK_GROWTH_SECS`, i.e. the user
/// actually hears audio after the scrub.
///
/// `AutoAdvanced` and `Stalled` are both production-bug shapes:
/// `AutoAdvanced` = silent track switch (false-EOF cascade);
/// `Stalled` = no track switch but the source pipeline froze after
/// the seek (reader stuck, codec wedged, scheduler not advancing).
/// The user-visible effect is the same — the track stops playing —
/// so the test must reject both.
#[derive(Debug)]
enum ScrubOutcome {
    PlaybackContinued {
        final_position: f64,
        position_growth: f64,
        chunks_after_seek: usize,
    },
    Stalled {
        final_position: Option<f64>,
        position_growth: Option<f64>,
        chunks_after_seek: usize,
        chunks_at_seek: usize,
    },
    AutoAdvanced {
        triggered_by: AdvanceTrigger,
        new_index: usize,
    },
}

#[derive(Debug, Clone, Copy)]
enum AdvanceTrigger {
    DidPlayToEnd,
    DidFail,
    NoTerminalEvent,
}

struct ScrubObservation<'a> {
    queue: &'a Queue,
    rx: &'a mut EventReceiver,
    recorder: &'a Recorder,
    event_log: &'a mut Vec<TimedEvent>,
    started_at: Instant,
    params: ScrubParams<'a>,
}

#[derive(Clone, Copy)]
struct ScrubParams<'a> {
    target_src: &'a str,
    chunks_at_seek: usize,
    min_new_chunks: usize,
    initial_index: Option<usize>,
    seek_target_secs: f64,
    min_growth_secs: f64,
    budget: Duration,
}

async fn observe_scrub_outcome(obs: ScrubObservation<'_>) -> ScrubOutcome {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    let ScrubParams {
        target_src,
        chunks_at_seek,
        min_new_chunks,
        initial_index,
        seek_target_secs,
        min_growth_secs,
        budget,
    } = obs.params;
    let deadline = Instant::now() + budget;
    let success_threshold = seek_target_secs + min_growth_secs;
    let mut last_terminal_for_target: Option<AdvanceTrigger> = None;
    loop {
        let current_index = obs.queue.current_index();
        if current_index != initial_index {
            return ScrubOutcome::AutoAdvanced {
                triggered_by: last_terminal_for_target.unwrap_or(AdvanceTrigger::NoTerminalEvent),
                new_index: current_index.unwrap_or(usize::MAX),
            };
        }
        let chunks_now = count_build_chunks(obs.recorder);
        let new_chunks = chunks_now.saturating_sub(chunks_at_seek);
        // Primary success signal: the decoder actually produced fresh
        // PCM chunks after the seek. Chunks are the ground truth (one
        // probe fires per decoded chunk handed to the audio pipeline);
        // `position_seconds` is OffSession-render-driven and can stay
        // `None` even when chunks are flowing. Both signals matter
        // when present, but chunk count alone is enough to prove the
        // engine resumed after the scrub.
        if new_chunks >= min_new_chunks {
            let pos = obs.queue.position_seconds();
            let growth_ok = pos.is_none_or(|p| p >= success_threshold);
            if growth_ok {
                return ScrubOutcome::PlaybackContinued {
                    final_position: pos.unwrap_or(seek_target_secs),
                    position_growth: pos.map_or(0.0, |p| p - seek_target_secs),
                    chunks_after_seek: new_chunks,
                };
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let final_position = obs.queue.position_seconds();
            let position_growth = final_position.map(|p| p - seek_target_secs);
            return ScrubOutcome::Stalled {
                final_position,
                position_growth,
                chunks_after_seek: new_chunks,
                chunks_at_seek,
            };
        }
        let recv_budget = remaining.min(Duration::from_millis(200));
        match timeout(recv_budget, obs.rx.recv()).await {
            Ok(Ok(ev)) => {
                if let Event::Player(PlayerEvent::ItemDidFail { src, .. }) = &ev
                    && src.as_ref() == target_src
                {
                    last_terminal_for_target = Some(AdvanceTrigger::DidFail);
                }
                if let Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. }) = &ev
                    && src.as_ref() == target_src
                {
                    last_terminal_for_target = Some(AdvanceTrigger::DidPlayToEnd);
                }
                obs.event_log.push(TimedEvent {
                    elapsed: obs.started_at.elapsed(),
                    event: ev,
                });
            }
            Ok(Err(RecvError::Lagged(n))) => {
                eprintln!("[event-log] LAGGED dropped {n} events");
                continue;
            }
            Ok(Err(RecvError::Closed)) => {
                let final_position = obs.queue.position_seconds();
                let position_growth = final_position.map(|p| p - seek_target_secs);
                return ScrubOutcome::Stalled {
                    final_position,
                    position_growth,
                    chunks_after_seek: new_chunks,
                    chunks_at_seek,
                };
            }
            Err(_) => continue,
        }
    }
}

/// Rapid-scrub regression: the user reproduces this by tapping a
/// track and dragging the slider before the first segment after the
/// scrub target is fetched. Pre-fix, the decoder error propagated as
/// `eof_seen=true` and the queue auto-advanced silently. Post-fix,
/// the same decode error surfaces as `ItemDidFail` and the queue
/// either keeps the track selected (if the failure is transient at
/// the source layer) or advances WITH a preceding `ItemDidFail`
/// (graceful-fail surface). The silent advance must never happen.
///
/// Why a multi-track queue: single-seek auto-advance is a no-op when
/// there is no successor to advance to (the queue stays parked at
/// `current_index == Some(0)` even after `ItemDidFail` fires). The
/// production cascade always runs against a populated queue; we
/// match that by prepending a sentinel "before" track and appending
/// a "after" track around the target, so a forward advance has
/// somewhere to land and the test can detect it via
/// `queue.current_index()` change.
///
/// Parametrised on `DecoderBackend` for coverage; both backends share
/// the same `kithara-audio` source FSM and `kithara-queue` advance
/// path, so the bug — wherever it lives in those layers — must
/// surface on both.
#[kithara::test(tokio)]
#[ignore = "needs KITHARA_DRM_PROD_* baked + prod CDN reachable — run with --run-ignored=only"]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn rapid_scrub_does_not_silently_advance(#[case] backend: DecoderBackend) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let recorder = install_recorder();
    let ctx = build_ctx().await;

    // Multi-track playlist that mirrors a real user session. The
    // sentinel tracks bracket the target so any single-step queue
    // advance (forward or backward) shows up as a `current_index`
    // delta — without them, the queue cannot move and the bug
    // silently disappears.
    const SENTINEL_BEFORE: &str =
        "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";
    const SENTINEL_AFTER: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/59232754_2/master.m3u8";

    let mut rx = ctx.queue.subscribe();

    let _before_id = ctx
        .queue
        .append(build_track_source(SENTINEL_BEFORE, &ctx, backend));
    let target_id = ctx
        .queue
        .append(build_track_source(TARGET_TRACK, &ctx, backend));
    let _after_id = ctx
        .queue
        .append(build_track_source(SENTINEL_AFTER, &ctx, backend));

    wait_for_loaded(&mut rx, &ctx.queue, target_id, LOAD_BUDGET)
        .await
        .unwrap_or_else(|e| panic!("Loaded never arrived for {TARGET_TRACK}: {e}"));

    ctx.queue
        .select(target_id, Transition::None)
        .expect("select target");

    // Production-truth warmup signal: the first PCM chunk has been
    // built. Anything earlier and the seek lands in a state the
    // production code never sees.
    wait_for_warmup(&recorder, WARMUP_PROBE_BUDGET)
        .await
        .unwrap_or_else(|e| panic!("warmup probe never fired for {TARGET_TRACK}: {e}"));

    let initial_index = ctx.queue.current_index();
    let initial_position = ctx.queue.position_seconds();
    eprintln!(
        "[harness] pre-scrub: current_index={initial_index:?} \
         current_track={:?} position={initial_position:?}s",
        ctx.queue.track(target_id).as_ref().map(|e| e.id.as_u64()),
    );

    let chunks_at_seek = count_build_chunks(&recorder);
    eprintln!("[harness] chunks_at_seek={chunks_at_seek}");

    ctx.queue.seek(SCRUB_TARGET_SECS).expect("seek");
    let started_at = Instant::now();
    let mut event_log: Vec<TimedEvent> = Vec::new();

    // Require at least 10 fresh decoded chunks after the seek. One
    // chunk is ~1024 frames @ 44.1 kHz ≈ 23 ms of PCM, so ten chunks
    // ≈ 230 ms of decoded audio — well below `MIN_POST_SEEK_GROWTH_SECS`
    // (1 s) and large enough that timeline-only drift can't fake it.
    const MIN_NEW_CHUNKS_AFTER_SEEK: usize = 10;

    let outcome = observe_scrub_outcome(ScrubObservation {
        queue: &ctx.queue,
        rx: &mut rx,
        recorder: &recorder,
        event_log: &mut event_log,
        started_at,
        params: ScrubParams {
            target_src: TARGET_TRACK,
            chunks_at_seek,
            min_new_chunks: MIN_NEW_CHUNKS_AFTER_SEEK,
            initial_index,
            seek_target_secs: SCRUB_TARGET_SECS,
            min_growth_secs: MIN_POST_SEEK_GROWTH_SECS,
            budget: OUTCOME_BUDGET,
        },
    })
    .await;

    eprintln!("[event-log] captured {} events:", event_log.len());
    for TimedEvent { elapsed, event } in &event_log {
        eprintln!("  [{:>6.2}s] {event:?}", elapsed.as_secs_f64());
    }

    let _ = initial_position;
    match outcome {
        ScrubOutcome::PlaybackContinued {
            final_position,
            position_growth,
            chunks_after_seek,
        } => {
            eprintln!(
                "[harness] playback continued past seek target. \
                 final_position={final_position:.2}s growth={position_growth:.2}s \
                 new_chunks={chunks_after_seek}"
            );
        }
        ScrubOutcome::Stalled {
            final_position,
            position_growth,
            chunks_after_seek,
            chunks_at_seek,
        } => {
            panic!(
                "PLAYBACK STALLED AFTER SEEK: user scrubbed to {SCRUB_TARGET_SECS}s and \
                 the queue stayed on the target track (no false auto-advance), but the \
                 decoder produced only {chunks_after_seek} fresh PCM chunks past the \
                 pre-seek count of {chunks_at_seek} within the {OUTCOME_BUDGET:?} budget. \
                 final_position={final_position:?}s growth={position_growth:?}s. \
                 The user hears silence: reader cursor, scheduler, or decoder failed to \
                 resume PCM production after the seek even though no terminal event fired. \
                 See the event log above for the last activity before the stall."
            );
        }
        ScrubOutcome::AutoAdvanced {
            triggered_by,
            new_index,
        } => {
            panic!(
                "PRODUCTION CASCADE REPRODUCED: a single user seek to {SCRUB_TARGET_SECS}s \
                 caused the queue to advance from index {initial_index:?} → {new_index} \
                 (triggered by {triggered_by:?}). The user never asked for a track switch. \
                 Root cause is in the audio source pipeline reporting a premature terminal \
                 condition after a failed cold-range seek — see the event log above. The \
                 queue auto-advance itself is correct policy; the bug is the source layer \
                 emitting `ItemDidPlayToEnd`/`ItemDidFail` instead of recovering or staying \
                 parked in `WaitingForSource`."
            );
        }
    }

    ctx.queue.clear();
    drop(ctx);
}
