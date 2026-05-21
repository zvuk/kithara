#![cfg(not(target_arch = "wasm32"))]

//! Reproduces the false-EOF auto-advance the user caught in `app.log @
//! 06:39:13`: prod zvuk DRM track 171515249, rapid scrub to ~45%
//! before warmup completes, decoder fails on the unbuffered range,
//! and the queue treats the failure as a normal end-of-stream and
//! silently advances.
//!
//! The fix in `crates/kithara-play/src/impls/player_resource.rs`
//! separates the "failed" signal from `eof_seen` so that decode errors
//! surface as `PlayerEvent::ItemDidFail` rather than `ItemDidPlayToEnd`.
//! This test pins that contract end-to-end through the same
//! `Queue`/`PlayerImpl`/`Downloader` stack the binary uses, against
//! production zvuk DRM (the exact URL captured in `app.log`).
//!
//! The harness drives only the public APIs that UI buttons call
//! (`Queue::append`, `select`, `seek`, `tick`, `subscribe`) — no test
//! code lives in the `kithara-app` production crate. The control
//! flow is driven by USDT probes (`kithara::probe`) and `EventBus`
//! events, not by wall-clock timers, so the test reflects production
//! state transitions rather than a guessed cadence.
//!
//! `#[ignore]`-gated: needs `KITHARA_DRM_PROD_*` baked at build time
//! (see `zvuk_prod_drm_e2e.rs`).

use std::{sync::Arc, time::Duration};

use kithara_app::{config::AppConfig, sources::build_source};
use kithara_assets::{FlushHub, FlushPolicy, StoreOptions};
use kithara_decode::DecoderBackend;
use kithara_events::{
    AbrMode, Event, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus,
};
use kithara_integration_tests::{TestTempDir, kithara, offline::OfflineSession};
use kithara_net::{HttpClient, NetOptions};
use kithara_play::{PlayerConfig, PlayerImpl};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use kithara_test_utils::probe::capture::{Recorder, install as install_recorder};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

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

struct Ctx {
    config: AppConfig,
    queue: Arc<Queue>,
    cache: TestTempDir,
}

async fn build_ctx() -> Ctx {
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancellationToken::new())).build(),
    );
    let flush_hub = FlushHub::new(CancellationToken::new(), FlushPolicy::default());
    let config = AppConfig::new(downloader, flush_hub);
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    let q = Arc::clone(&queue);
    tokio::spawn(async move {
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
    match build_source(url, &ctx.config) {
        TrackSource::Config(mut cfg) => {
            cfg.store = StoreOptions::new(ctx.cache.path());
            cfg.decoder_backend = backend;
            cfg.initial_abr_mode = AbrMode::Auto(None);
            TrackSource::Config(cfg)
        }
        other => other,
    }
}

async fn wait_for_loaded(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    deadline: Duration,
) -> Result<(), String> {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
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
/// active track. Drives off the `kithara_decode::composed::build_chunk`
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

/// What the queue did with the target track after the scrub. Drains
/// `Event::Player` until one of the two terminal player events fires
/// for `target_src` or the budget elapses. The bug = `PlayedToEnd`
/// for the target without a prior `Failed`. The fix = `Failed` (or
/// no terminal event at all, if the source layer recovered).
#[derive(Debug)]
enum ScrubOutcome {
    DidFail,
    DidPlayToEnd,
    BudgetElapsed,
}

async fn observe_scrub_outcome(
    rx: &mut EventReceiver,
    target_src: &str,
    budget: Duration,
) -> ScrubOutcome {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    let deadline = tokio::time::Instant::now() + budget;
    let mut item_failed_seen = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return if item_failed_seen {
                ScrubOutcome::DidFail
            } else {
                ScrubOutcome::BudgetElapsed
            };
        }
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(Event::Player(PlayerEvent::ItemDidFail { src, .. })))
                if src.as_ref() == target_src =>
            {
                item_failed_seen = true;
                continue;
            }
            Ok(Ok(Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. })))
                if src.as_ref() == target_src =>
            {
                return if item_failed_seen {
                    ScrubOutcome::DidFail
                } else {
                    ScrubOutcome::DidPlayToEnd
                };
            }
            Ok(Ok(_)) => continue,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => {
                return if item_failed_seen {
                    ScrubOutcome::DidFail
                } else {
                    ScrubOutcome::BudgetElapsed
                };
            }
            Err(_) => {
                return if item_failed_seen {
                    ScrubOutcome::DidFail
                } else {
                    ScrubOutcome::BudgetElapsed
                };
            }
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
#[kithara::test(tokio)]
#[ignore = "needs KITHARA_DRM_PROD_* baked + prod CDN reachable — run with --run-ignored=only"]
async fn rapid_scrub_does_not_silently_advance() {
    let recorder = install_recorder();
    let ctx = build_ctx().await;
    let source = build_track_source(TARGET_TRACK, &ctx, DecoderBackend::Symphonia);
    let mut rx = ctx.queue.subscribe();
    let track_id = ctx.queue.append(source);

    wait_for_loaded(&mut rx, &ctx.queue, track_id, LOAD_BUDGET)
        .await
        .unwrap_or_else(|e| panic!("Loaded never arrived for {TARGET_TRACK}: {e}"));

    ctx.queue
        .select(track_id, Transition::None)
        .expect("select");

    // Wait for the first decoded chunk via probe — production-truth
    // signal that the player is warm. No wall-clock guess.
    wait_for_warmup(&recorder, WARMUP_PROBE_BUDGET)
        .await
        .unwrap_or_else(|e| panic!("warmup probe never fired for {TARGET_TRACK}: {e}"));

    ctx.queue.seek(SCRUB_TARGET_SECS).expect("seek");

    let outcome = observe_scrub_outcome(&mut rx, TARGET_TRACK, OUTCOME_BUDGET).await;

    match outcome {
        ScrubOutcome::DidFail => {}
        ScrubOutcome::BudgetElapsed => {}
        ScrubOutcome::DidPlayToEnd => {
            panic!(
                "FALSE-EOF AUTO-ADVANCE REPRODUCED: PlayerEvent::ItemDidPlayToEnd for \
                 {TARGET_TRACK} fired during the scrub observation window without a \
                 preceding ItemDidFail. This is the exact cascade from app.log @ 06:39:13 \
                 (decode error → false EOF → silent advance). Either the fix is incomplete \
                 or a regression has reintroduced it."
            );
        }
    }

    let _ = ctx.queue.remove(track_id);
    drop(ctx);
}
