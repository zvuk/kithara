#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use kithara::{
    events::{
        AbrMode, AudioEvent, Event, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus,
    },
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, Instant, sleep, timeout},
        tokio,
        traits::FromWithParams,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, EncryptionRequest},
    kithara,
    offline::OfflineSession,
    temp_dir,
};

/// Track shape: 30 segments × 4 s = 120 s. Long enough that 50 % and
/// 90 % targets land in distinct cold regions.
const SEGMENT_COUNT_U32: u32 = 30;
const SEGMENT_COUNT: usize = SEGMENT_COUNT_U32 as usize;
const SEGMENT_DURATION_S: f64 = 4.0;
/// Per-segment delay applied to every fetch. Mirrors a realistic
/// CDN RTT — comfortably above the audio worker's 10 ms
/// `WAIT_RANGE_TIMEOUT` so the scheduler cannot transparently
/// satisfy a sudden seek.
const CDN_DELAY_MS: u64 = 120;

/// Playback between sequential scrubs. Long enough that the audio
/// worker actually exits the post-first-seek `WaitingForSource`
/// state and renders real PCM.
const PLAY_BETWEEN_SCRUBS: Duration = Duration::from_secs(2);

/// Total track duration derived from the fixture, used to compute
/// scrub targets and to cap the per-seek observation budget.
// `u32 as f64` is precision-safe (`u32::MAX < 2^53`) and works in
// const context, unlike `f64::from(u32)` which is not yet const.
const TRACK_DURATION_S: f64 = SEGMENT_COUNT_U32 as f64 * SEGMENT_DURATION_S;

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
    let start = Instant::now();
    while start.elapsed() < budget {
        match timeout(Duration::from_millis(200), rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
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
    /// Engine surfaced `ItemDidFail` for the scrubbed track — the bug.
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
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    let deadline = Instant::now() + budget;
    loop {
        if let Some(pos) = queue.position_seconds()
            && pos > seek_target + 0.5
        {
            return ScrubOutcome::Landed { reached: pos };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ScrubOutcome::BudgetElapsed {
                last_position: queue.position_seconds(),
            };
        }
        let recv_budget = remaining.min(Duration::from_millis(200));
        match timeout(recv_budget, rx.recv())
            .await
            .map(|r| r.map(|env| env.event))
        {
            Ok(Ok(Event::Player(PlayerEvent::ItemDidFail { src, .. })))
                if src.as_ref() == target_src =>
            {
                return ScrubOutcome::ItemDidFail {
                    src: src.to_string(),
                };
            }
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => {
                return ScrubOutcome::BudgetElapsed {
                    last_position: queue.position_seconds(),
                };
            }
            Err(_) => continue,
        }
    }
}

/// Wait until the playback sink commits real PCM progress past
/// `baseline_secs` — i.e. the audio worker has left the post-seek
/// `WaitingForSource` state and is rendering again. This is the
/// concrete state the old fixed inter-scrub sleep was approximating:
/// "play between scrubs so recovery actually runs and real PCM is
/// rendered". `budget` is a safety deadline that bounds a hang, not a
/// pacing wait — the function returns the moment progress is observed.
async fn wait_for_playback_progress(
    rx: &mut EventReceiver,
    baseline_secs: f64,
    budget: Duration,
) -> Option<f64> {
    use kithara::platform::tokio::sync::broadcast::error::RecvError;
    let fut = async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Audio(AudioEvent::PlaybackProgress { position_ms, .. })) => {
                    let pos_secs = position_ms as f64 / 1000.0;
                    if pos_secs > baseline_secs {
                        return Some(pos_secs);
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    };
    timeout(budget, fut).await.ok().flatten()
}

struct Harness {
    queue: Arc<Queue>,
    rx: EventReceiver,
    tick: tokio::task::JoinHandle<()>,
    master_url: String,
}

impl Harness {
    /// Simulate a real slider drag: emit a burst of intermediate
    /// seeks racing toward `final_target` over `drag_duration`. Each
    /// intermediate seek lands in a cold range and starts a new
    /// epoch before the previous one's `decoder.seek` could possibly
    /// have completed — this is what causes the prod cascade where
    /// a stale `decoder.seek` partially read an atom header, the
    /// new epoch arrived mid-atom, and `next_chunk` then saw the
    /// torn cursor as "isomp4: no atom pending read".
    async fn drag(&mut self, final_target: f64, drag_duration: Duration, steps: usize, tag: &str) {
        let start_pos = self.queue.position_seconds().unwrap_or(0.0);
        let steps_u32 = u32::try_from(steps).unwrap_or(u32::MAX);
        let step_delay = drag_duration / steps_u32;
        for step in 1..=steps_u32 {
            let frac = f64::from(step) / f64::from(steps_u32);
            let target = start_pos + (final_target - start_pos) * frac;
            self.queue.seek(target).expect("drag seek accepted");
            eprintln!("[{tag}] drag step {step}/{steps_u32} → {target:.2}s");
            sleep(step_delay).await;
        }
    }

    /// Issue a seek and observe outcome — returns the outcome so
    /// each scenario can pattern-match its own acceptance criteria.
    async fn scrub(&mut self, target: f64, tag: &str) -> ScrubOutcome {
        self.queue.seek(target).expect("seek accepted");
        eprintln!("[{tag}] seek issued target={target:.2}s");
        observe_scrub_outcome(
            &self.queue,
            &mut self.rx,
            &self.master_url,
            target,
            SEEK_OBSERVE_BUDGET,
        )
        .await
    }

    async fn setup(temp_dir: &TestTempDir) -> Self {
        // AES-128 key + IV matching `packaged_encrypted_builder` in
        // `tests/src/hls_server.rs` — every kithara fixture uses these
        // same constants so the integration helpers can verify
        const AES128_KEY: [u8; 16] = *b"0123456789abcdef";
        const AES128_IV: [u8; 16] = [0u8; 16];
        let key_hex: String = AES128_KEY.iter().map(|b| format!("{b:02x}")).collect();
        let iv_hex: String = AES128_IV.iter().map(|b| format!("{b:02x}")).collect();

        let helper = TestServerHelper::new().await;
        // Mirror prod zvuk DRM shape: 4 variants (slq / smq / shq /
        // slossless analogue), AES-128 CBC, ABR=Auto, per-segment
        // delay above the audio worker's wait budget. Slow variant 0
        // has higher delay so the ABR controller is tempted to
        // upgrade — same as the prod trace where `commit_variant_switch
        // reason=UpSwitch from_variant=0 to_variant=3` fires
        let builder = HlsFixtureBuilder::new()
            .variant_count(4)
            .segments_per_variant(SEGMENT_COUNT)
            .segment_duration_secs(SEGMENT_DURATION_S)
            .variant_bandwidths(vec![300_000, 800_000, 1_800_000, 5_000_000])
            // Codec choice mirrors prod (`aac2` = HE-AAC v2 with
            // SBR + Parametric Stereo). The cascade does not
            // reproduce on AAC-LC — Symphonia's fmp4 demuxer
            // appears to handle plain mp4a fragments fine but
            // mis-handles the moof layout that the HE-AAC v2
            // fixtures (and prod zvuk DRM streams) generate, so
            // the reproducer must use the same codec the bug
            .packaged_audio_aac_he_v2(44_100, 2)
            .encryption(EncryptionRequest {
                key_hex,
                iv_hex: Some(iv_hex),
            })
            .push_delay_rule(DelayRule {
                variant: Some(0),
                delay_ms: 300,
                ..DelayRule::default()
            })
            .push_delay_rule(DelayRule {
                delay_ms: CDN_DELAY_MS,
                ..DelayRule::default()
            });
        let created = helper
            .create_hls(builder)
            .await
            .expect("create multi-variant HLS fixture");
        let master = created.master_url();
        let master_url = master.as_str().to_string();

        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(
                NetOptions::default(),
                CancelToken::never(),
            ))
            .build(),
        );
        let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
        let player = Arc::new(PlayerImpl::new(
            PlayerConfig::builder()
                .byte_pool(kithara::bufpool::BytePool::default())
                .pcm_pool(kithara::bufpool::PcmPool::default())
                .session(OfflineSession::arc_auto())
                .build(),
        ));
        let queue = Arc::new(Queue::build(player, QueueConfig::default()));

        let queue_for_tick = Arc::clone(&queue);
        let tick = tokio::task::spawn(async move {
            loop {
                sleep(Duration::from_millis(50)).await;
                if queue_for_tick.tick().is_err() {
                    break;
                }
            }
        });

        let cfg = ResourceConfig::for_src(master.as_str())
            .expect("valid URL")
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .downloader(downloader)
            .store(store)
            .initial_abr_mode(AbrMode::Auto(None))
            .build();
        let mut rx = queue.subscribe();
        let id = queue.append(TrackSource::Config(Box::new(cfg)));

        wait_for_status(&mut rx, &queue, id, TrackStatus::Loaded, LOAD_BUDGET)
            .await
            .unwrap_or_else(|e| panic!("Loaded never arrived: {e}"));
        queue.select(id, Transition::None).expect("select");
        queue.play();

        Self {
            queue,
            rx,
            master_url,
            tick,
        }
    }

    fn shutdown(self) {
        self.tick.abort();
    }
}

fn assert_not_failed(outcome: ScrubOutcome, target: f64, tag: &str) {
    match outcome {
        ScrubOutcome::Landed { reached } => {
            eprintln!("[{tag}] landed at {reached:.2}s — root recovery worked");
        }
        ScrubOutcome::BudgetElapsed { last_position } => {
            eprintln!(
                "[{tag}] inconclusive: budget elapsed, last position={last_position:?} — \
                 still buffering, not a regression"
            );
        }
        ScrubOutcome::ItemDidFail { src } => {
            panic!(
                "[{tag}] CRASHED: PlayerEvent::ItemDidFail fired for {src} after seek to \
                 {target:.2}s. Decoder seek into cold byte range corrupted the demuxer \
                 cursor — next_chunk surfaced `isomp4: no atom pending read`, pcm channel \
                 closed, queue silently advanced. Mirrors prod app.log cascade."
            );
        }
    }
}

/// Parametrised cold-range seek reproducer.
///
/// Cases:
/// - `single_mid` — single clean scrub to ~40 % of an unbuffered
///   track (prod `50984034_1`: pos=79.9 s / dur=194 s).
/// - `near_end` — single clean scrub to 95 % (bug #7; covers the
///   `171515249_1` shape from app.log).
/// - `fwd_then_back` — forward to 80 % then backward to 20 %
///   (bug #6: the demuxer cursor must survive a cold forward seek
///   without poisoning the subsequent backward seek).
/// - `rapid_50_then_90` — two sequential forward scrubs into
///   distinct cold regions, the original `app.log @ 09:43:50`
///   shape.
/// - `drag_mid` / `drag_end` — burst of 8 seeks over 400 ms
///   ending at 40 % / 95 %. Mirrors a real slider drag where each
///   intermediate seek starts a fresh epoch before the previous
///   `decoder.seek` could finish; this is the cascade shape that
///   crashed three prod tracks in `app.log @ 11:50` and is harder
///   to dodge with a passive scheduler warm-up.
///
/// `second_ratio = None` skips the second seek; otherwise the
/// harness waits between the two for the playback sink to commit
/// real PCM progress past the first landing point — bounded by
/// `PLAY_BETWEEN_SCRUBS` as a safety deadline — so any post-first-seek
/// recovery actually runs. Each case asserts no
/// `PlayerEvent::ItemDidFail` after the final scrub.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::single_mid(0.40, None)]
#[case::near_end(0.95, None)]
#[case::fwd_then_back(0.80, Some(0.20))]
#[case::rapid_50_then_90(0.50, Some(0.90))]
async fn seek_into_cold_range_does_not_fail(
    temp_dir: TestTempDir,
    #[case] first_ratio: f64,
    #[case] second_ratio: Option<f64>,
) {
    let mut harness = Harness::setup(&temp_dir).await;

    let first_target = TRACK_DURATION_S * first_ratio;
    let first_tag = if second_ratio.is_some() {
        "first"
    } else {
        "only"
    };
    let first_outcome = harness.scrub(first_target, first_tag).await;

    let Some(second_ratio) = second_ratio else {
        harness.shutdown();
        assert_not_failed(first_outcome, first_target, first_tag);
        return;
    };

    if let ScrubOutcome::ItemDidFail { src } = first_outcome {
        harness.shutdown();
        panic!("[first] CRASHED before second scrub (src={src})");
    }

    // Between scrubs, wait for the audio worker to actually render real
    // PCM past where the first seek landed — not a fixed timer. This is
    // the state the second scrub needs: post-first-seek recovery has run
    // and playback is progressing again. `PLAY_BETWEEN_SCRUBS` is the
    // safety deadline bounding a stall, not a pacing wait.
    let baseline_secs = harness.queue.position_seconds().unwrap_or(0.0);
    wait_for_playback_progress(&mut harness.rx, baseline_secs, PLAY_BETWEEN_SCRUBS).await;

    let second_target = TRACK_DURATION_S * second_ratio;
    let second_outcome = harness.scrub(second_target, "second").await;
    harness.shutdown();
    assert_not_failed(second_outcome, second_target, "second");
}

/// Slider-drag reproducer. 8 intermediate seeks fired over 400 ms,
/// each starting a new epoch before the previous `decoder.seek`
/// could finish. The final seek's position lands in a cold range
/// the scheduler has not reached, while the demuxer is still
/// mid-atom from one of the racing earlier seeks — the exact
/// torn-cursor shape three prod tracks crashed on in `app.log
/// @ 11:50`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::drag_mid(0.40)]
#[case::drag_end(0.95)]
async fn seek_drag_into_cold_range_does_not_fail(temp_dir: TestTempDir, #[case] final_ratio: f64) {
    const DRAG_DURATION: Duration = Duration::from_millis(400);
    const DRAG_STEPS: usize = 8;

    let mut harness = Harness::setup(&temp_dir).await;
    let final_target = TRACK_DURATION_S * final_ratio;

    harness
        .drag(final_target, DRAG_DURATION, DRAG_STEPS, "drag")
        .await;

    let outcome = observe_scrub_outcome(
        &harness.queue,
        &mut harness.rx,
        &harness.master_url,
        final_target,
        SEEK_OBSERVE_BUDGET,
    )
    .await;
    harness.shutdown();
    assert_not_failed(outcome, final_target, "drag");
}
