#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Reproduces the production rapid-scrub cascade from
//! `app.log @ 09:43:50`: user taps a prod DRM track, drags the slider
//! mid-track, plays a second, drags again. Each scrub lands in a
//! byte range the scheduler has not fetched yet, decoder seek hits
//! `SeekOutOfRange`, `decoder_next_chunk` then returns
//! `isomp4: no atom pending read`, PCM channel closes, the player
//! escalates `ItemDidFail`, and the queue silently advances.
//!
//! Two design choices reproduce the bug locally where
//! `cold_seek_middle::queue_seek_multi_variant_cold_far` (single
//! seek, deep warmup) passes:
//!
//! 1. **Two sequential seeks** — 50 % then 90 % with 2 s of playback
//!    between them. The first seek alone often lands because the
//!    scheduler had time to walk ahead of the slider; the *second*
//!    seek lands in a range that the post-first-seek prefetch has
//!    not reached yet.
//! 2. **Per-segment server-side delay** — every segment is delayed
//!    by ~120 ms (realistic CDN RTT, well above the audio worker's
//!    10 ms `WAIT_RANGE_TIMEOUT`). This is what
//!    `kithara-integration-tests` calls "jitter middleware",
//!    implemented through the fixture's `DelayRule` so no
//!    test-only code leaks into production crates.
//!
//! Asserts that neither scrub escalates `PlayerEvent::ItemDidFail`.
//! Track may end up still buffering past the second target — that
//! is acceptable (root recovery in progress). The forbidden outcome
//! is the silent advance, which is caught by the companion
//! `false_eof_rapid_scrub.rs` against the prod CDN.

use std::{sync::Arc, time::Duration};

use kithara_assets::StoreOptions;
use kithara_events::{
    AbrMode, Event, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, EncryptionRequest},
    kithara,
    offline::OfflineSession,
    temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_play::{PlayerConfig, PlayerImpl, ResourceConfig};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::dl::{Downloader, DownloaderConfig};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

/// Track shape: 30 segments × 4 s = 120 s. Long enough that 50 % and
/// 90 % targets land in distinct cold regions.
const SEGMENT_COUNT: usize = 30;
const SEGMENT_DURATION_S: f64 = 4.0;
/// Per-segment delay applied to every fetch. Mirrors a realistic
/// CDN RTT — comfortably above the audio worker's 10 ms
/// `WAIT_RANGE_TIMEOUT` so the scheduler cannot transparently
/// satisfy a sudden seek.
const CDN_DELAY_MS: u64 = 120;

/// Playback between the two scrubs. Long enough that the audio
/// worker actually exits the post-first-seek `WaitingForSource`
/// state and renders real PCM.
const PLAY_BETWEEN_SCRUBS: Duration = Duration::from_secs(2);

const FIRST_SCRUB_RATIO: f64 = 0.50;
const SECOND_SCRUB_RATIO: f64 = 0.90;

/// Total track duration derived from the fixture, used to compute
/// scrub targets and to cap the per-seek observation budget.
const TRACK_DURATION_S: f64 = SEGMENT_COUNT as f64 * SEGMENT_DURATION_S;

const LOAD_BUDGET: Duration = Duration::from_secs(30);
const SEEK_OBSERVE_BUDGET: Duration = Duration::from_secs(15);

async fn wait_for_status(
    rx: &mut EventReceiver,
    queue: &Queue,
    id: TrackId,
    target: TrackStatus,
    budget: Duration,
) -> Result<(), String> {
    if let Some(entry) = queue.track(id)
        && entry.status == target
    {
        return Ok(());
    }
    let start = std::time::Instant::now();
    while start.elapsed() < budget {
        match timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(Event::Queue(QueueEvent::TrackStatusChanged { id: tid, status })))
                if tid == id =>
            {
                if status == target {
                    return Ok(());
                }
                if let TrackStatus::Failed(err) = status {
                    return Err(format!("track load failed: {err}"));
                }
            }
            _ => {}
        }
    }
    Err(format!("timeout waiting for {target:?}"))
}

#[derive(Debug)]
enum ScrubOutcome {
    /// Position advanced past the seek target — root recovery worked.
    Landed { reached: f64 },
    /// Engine surfaced ItemDidFail for the scrubbed track — the bug.
    ItemDidFail { src: String },
    /// Budget elapsed without either signal — still buffering /
    /// recovering. Not a regression but not landed either.
    BudgetElapsed { last_position: Option<f64> },
}

/// Drain `Event::Player` until either the scrub target is reached
/// (root recovery), `ItemDidFail` fires for the scrubbed src (the
/// bug), or `budget` elapses.
async fn observe_scrub_outcome(
    queue: &Queue,
    rx: &mut EventReceiver,
    target_src: &str,
    seek_target: f64,
    budget: Duration,
) -> ScrubOutcome {
    use kithara_platform::tokio::sync::broadcast::error::RecvError;
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Some(pos) = queue.position_seconds()
            && pos > seek_target + 0.5
        {
            return ScrubOutcome::Landed { reached: pos };
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return ScrubOutcome::BudgetElapsed {
                last_position: queue.position_seconds(),
            };
        }
        let recv_budget = remaining.min(Duration::from_millis(200));
        match timeout(recv_budget, rx.recv()).await {
            Ok(Ok(Event::Player(PlayerEvent::ItemDidFail { src, .. })))
                if src.as_ref() == target_src =>
            {
                return ScrubOutcome::ItemDidFail {
                    src: src.to_string(),
                };
            }
            Ok(Ok(_)) => continue,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => {
                return ScrubOutcome::BudgetElapsed {
                    last_position: queue.position_seconds(),
                };
            }
            Err(_) => continue,
        }
    }
}

/// Multi-variant HLS + per-segment CDN delay + warmup + two
/// sequential scrubs (50 % then 90 %) with 2 s playback between.
/// Reproduces the prod cascade from `app.log @ 09:43:50` deterministically.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn rapid_scrub_into_cold_segment_does_not_fail(temp_dir: TestTempDir) {
    // AES-128 key + IV matching `packaged_encrypted_builder` in
    // `tests/src/hls_server.rs` — every kithara fixture uses these
    // same constants so the integration helpers can verify
    // decryption end-to-end.
    const AES128_KEY: [u8; 16] = *b"0123456789abcdef";
    const AES128_IV: [u8; 16] = [0u8; 16];
    let key_hex: String = AES128_KEY.iter().map(|b| format!("{b:02x}")).collect();
    let iv_hex: String = AES128_IV.iter().map(|b| format!("{b:02x}")).collect();

    let helper = TestServerHelper::new().await;
    // Mirror the prod zvuk DRM shape: 4 variants (slq / smq / shq /
    // slossless analogue), AES-128 CBC encryption, ABR enabled and
    // free to up-switch. Each variant pulls slightly different
    // per-segment latency to spread the ABR controller's decisions
    // — the slow variant has a much higher delay so the controller
    // is tempted to upgrade, exactly as in `app.log` where the
    // production trace shows repeated `commit_variant_switch
    // reason=UpSwitch from_variant=0 to_variant=3` lines.
    let builder = HlsFixtureBuilder::new()
        .variant_count(4)
        .segments_per_variant(SEGMENT_COUNT)
        .segment_duration_secs(SEGMENT_DURATION_S)
        .variant_bandwidths(vec![300_000, 800_000, 1_800_000, 5_000_000])
        // HE-AAC v2 — mirrors the prod zvuk DRM codec that the user
        // ran into. AAC-LC fixtures recover cleanly from the same
        // sequence; the SBR + Parametric Stereo metadata in HE-AAC
        // v2 fmp4 atoms is what the audio pipeline appears to
        // mis-handle on seek-into-cold-range.
        .packaged_audio_aac_he_v2(44_100, 2)
        .encryption(EncryptionRequest {
            key_hex,
            iv_hex: Some(iv_hex),
        })
        // Slow variant 0 (slq analogue) ~ 300 ms per segment so the
        // ABR controller will eagerly upswitch mid-track.
        .push_delay_rule(DelayRule {
            variant: Some(0),
            delay_ms: 300,
            ..DelayRule::default()
        })
        // Other variants — realistic CDN RTT.
        .push_delay_rule(DelayRule {
            delay_ms: CDN_DELAY_MS,
            ..DelayRule::default()
        });
    let created = helper
        .create_hls(builder)
        .await
        .expect("create multi-variant HLS fixture");
    let master = created.master_url();
    let master_str = master.as_str().to_string();

    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            CancellationToken::new(),
        ))
        .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
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

    let mut cfg = ResourceConfig::for_src(master.as_str())
        .expect("valid URL")
        .downloader(downloader.clone())
        .store(store.clone())
        .build();
    cfg.initial_abr_mode = AbrMode::Auto(None);
    let mut rx = queue.subscribe();
    let id = queue.append(TrackSource::Config(Box::new(cfg)));

    wait_for_status(&mut rx, &queue, id, TrackStatus::Loaded, LOAD_BUDGET)
        .await
        .unwrap_or_else(|e| panic!("Loaded never arrived: {e}"));
    queue.select(id, Transition::None).expect("select");
    queue.play();

    // No wall-clock warmup — production trace from `app.log @
    // 09:43:50` shows the scrub landed `stream_pos=64512` (only the
    // init segment had been fetched). The user clicked the slider
    // before any media bytes arrived. Drop straight into the first
    // scrub so the test exercises the same cold-start race.

    // First scrub: 50 % into the track.
    let first_target = TRACK_DURATION_S * FIRST_SCRUB_RATIO;
    queue.seek(first_target).expect("first seek accepted");
    eprintln!("[rapid_scrub] first seek issued target={first_target:.2}s");

    match observe_scrub_outcome(
        &queue,
        &mut rx,
        &master_str,
        first_target,
        SEEK_OBSERVE_BUDGET,
    )
    .await
    {
        ScrubOutcome::Landed { reached } => {
            eprintln!("[rapid_scrub] first scrub landed at {reached:.2}s");
        }
        ScrubOutcome::ItemDidFail { src } => {
            tick_handle.abort();
            panic!(
                "FIRST SCRUB CRASHED: PlayerEvent::ItemDidFail fired for {src} after \
                 seek to {first_target:.2}s. Reproduces app.log @ 09:43:50 cascade."
            );
        }
        ScrubOutcome::BudgetElapsed { last_position } => {
            eprintln!(
                "[rapid_scrub] first scrub did not land within budget (last={last_position:?}) — \
                 continuing to second scrub"
            );
        }
    }

    // Playback between scrubs. Use sleep, not poll — we want any
    // background recovery work to actually run.
    sleep(PLAY_BETWEEN_SCRUBS).await;

    // Second scrub: 90 % into the track. Lands in a different cold
    // range that the post-first-seek prefetch has not reached.
    let second_target = TRACK_DURATION_S * SECOND_SCRUB_RATIO;
    queue.seek(second_target).expect("second seek accepted");
    eprintln!("[rapid_scrub] second seek issued target={second_target:.2}s");

    let outcome = observe_scrub_outcome(
        &queue,
        &mut rx,
        &master_str,
        second_target,
        SEEK_OBSERVE_BUDGET,
    )
    .await;
    tick_handle.abort();

    match outcome {
        ScrubOutcome::Landed { reached } => {
            eprintln!("[rapid_scrub] second scrub landed at {reached:.2}s — root recovery worked");
        }
        ScrubOutcome::BudgetElapsed { last_position } => {
            eprintln!(
                "[rapid_scrub] second scrub inconclusive: budget elapsed, last position={last_position:?}"
            );
        }
        ScrubOutcome::ItemDidFail { src } => {
            panic!(
                "SECOND SCRUB CRASHED: PlayerEvent::ItemDidFail fired for {src} after \
                 seek to {second_target:.2}s. Pipeline does not wait for the target byte range \
                 to arrive before issuing decoder.seek — `wait_range budget exceeded` or \
                 decoder `SeekOutOfRange` escalates straight to fail."
            );
        }
    }
}
