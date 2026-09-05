#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! The user-simulation scenarios that drive real production tracks: HE-AAC v2
//! behind AES-128 with per-segment key signing, reached over the live CDN with
//! credentials baked at build time. Reproduces by script what the user
//! reproduces by hand in the GUI.
//!
//! Compiled only into `suite_network`, which needs the `network` feature.
//! Everything here is on the public internet: the scenario that reached the
//! corporate slicer moved out with the rest of what CI cannot serve.
use kithara::{
    assets::{AssetStore, FlushHub, FlushPolicy, StorageBackend},
    decode::DecoderBackend,
    events::AbrMode,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, sleep},
        tokio,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_app::{
    config::{AppConfig, AppDrm},
    document::Config,
    pools::{AppPools, PoolsSection, build as app_pools},
};
use kithara_integration_tests::{
    TestTempDir, kithara,
    offline::OfflineQueue,
    user_sim::{actions::Action, scenarios},
};

/// Production zvuk DRM track URL — same one `zvuk_prod_drm_e2e.rs`
/// runs end-to-end. HE-AAC v2 fragments behind AES-128 + per-segment
/// X-Encrypted-Key signing; pinned because this is what the user
/// catches the seek bugs on manually.
const PROD_DRM_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";
/// Second prod DRM track — exercises the same provider but a
/// different track id, in case the bug is content-specific. URL
/// shape sourced from `app.yaml` playlist.
const PROD_DRM_TRACK_ALT: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8";

/// Build a prod-DRM track via the same `kithara-app` source resolver
/// the binary uses. The resolver picks up baked credentials and the
/// `zvuk-prod` keyserver provider.
fn prod_drm_spec(url: &str, ctx: &ProdCtx) -> TrackSource<AppPools> {
    crate::kithara_queue::app_track_source(
        url,
        &ctx.config,
        crate::kithara_queue::app_disk_asset_store(&ctx.config, ctx.cache.path()),
        DecoderBackend::Symphonia,
        AbrMode::Auto(None),
        None,
    )
}

struct ProdCtx {
    config: AppConfig,
    cache: TestTempDir,
}

fn build_prod_ctx() -> ProdCtx {
    let pools = app_pools(&PoolsSection::default()).expect("build app pool region");
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, pools.clone(), CancelToken::never()))
            .build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let shutdown = CancelToken::never();
    let document = Config::load(None, None).expect("the shipped configuration loads");
    let store = AssetStore::builder(pools.clone())
        .cancel(shutdown.child())
        .backend(StorageBackend::default())
        .flush_hub(flush_hub)
        .layouts(document.asset_layouts())
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools)
            .cancel(shutdown.child())
            .build(),
    );
    let config = AppConfig::builder()
        .drm(AppDrm::new(
            document
                .drm_policy()
                .expect("the shipped providers are valid"),
        ))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build();
    ProdCtx {
        config,
        cache: TestTempDir::new(),
    }
}

fn prod_queue(prod: &ProdCtx, pacing: Option<Duration>) -> OfflineQueue<AppPools> {
    let session = HostConfig::offline(prod.config.worker.pools().clone())
        .maybe_pacing(pacing)
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(prod.config.worker.clone())
            .build(),
    );
    OfflineQueue::new(
        session,
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .expect("create product offline queue")
}

async fn run_prod_drm_scenario(url: &str, actions: Vec<Action>) {
    let prod = build_prod_ctx();
    let queue = prod_queue(&prod, Some(Duration::from_millis(10)));
    let q_for_tick = queue.control();
    let tick = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue
        .append(prod_drm_spec(url, &prod))
        .expect("append production DRM track");

    // Use the same harness assertions but skip the per-track-cache
    // bootstrap by driving the queue directly here — production
    // tracks are auth-gated so we need the `kithara-app` source
    // resolver, not `ResourceConfig::for_src`.
    use kithara_integration_tests::user_sim::harness::{
        wait_for_loaded, wait_for_position_at_least,
    };
    wait_for_loaded(&queue, track_id, Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| panic!("prod DRM load fail: {e}"));
    queue
        .select(track_id, Transition::None)
        .expect("select prod DRM");
    wait_for_position_at_least(&queue, 1.0, Duration::from_secs(20))
        .await
        .unwrap_or_else(|e| panic!("prod DRM play fail: {e}"));

    // Apply actions directly via the queue — bypass SimHarness because
    // it's wired around the offline-fixture builder. The assertions
    // mirror the ones in `harness.rs` but live inline here.
    for action in actions {
        apply_action_to_queue(&queue, &action).await;
    }

    tick.abort();
    let _ = tick.await;
}

async fn apply_action_to_queue(queue: &QueueControl<AppPools>, action: &Action) {
    use kithara::play::SeekOutcome;
    let label = action.label();
    let duration = queue.duration_seconds().unwrap_or(0.0);
    assert!(duration > 0.0, "[{label}] duration unknown");
    match action {
        Action::SeekRatio(r) | Action::SeekNearEnd(r) => {
            let target = (duration * r).clamp(0.0, duration);
            let pre_track = queue.current().map(|e| e.id);
            let outcome = queue
                .seek(target)
                .unwrap_or_else(|e| panic!("[{label}] seek Err: {e}"));
            if matches!(outcome, SeekOutcome::PastEof { .. }) {
                return;
            }
            let started = kithara::platform::time::Instant::now();
            let budget = Duration::from_secs(10);
            let mut landed = false;
            while started.elapsed() < budget {
                if let Some(pos) = queue.position_seconds()
                    && (pos - target).abs() <= 2.0
                {
                    landed = true;
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
            assert!(
                landed,
                "[{label}] HANG: prod DRM seek to {target:.2}s never \
                 settled within {budget:?} (pos={:?}, dur={duration:.2}s)",
                queue.position_seconds()
            );
            if queue.current().map(|e| e.id) != pre_track {
                let pos_after = queue.position_seconds().unwrap_or(0.0);
                // SeekNearEnd close to dur is allowed to roll natural EOF
                if !matches!(action, Action::SeekNearEnd(_)) || (duration - pos_after).abs() > 5.0 {
                    panic!(
                        "[{label}] SPURIOUS AUTO-ADVANCE on prod DRM: \
                         track flipped (target={target:.2}s, pos={pos_after:.2}s, \
                         dur={duration:.2}s)"
                    );
                }
            }
        }
        Action::PlayFor(d) => {
            let pre = queue.position_seconds().unwrap_or(0.0);
            sleep(*d).await;
            let post = queue.position_seconds().unwrap_or(0.0);
            let advance = post - pre;
            let target = d.as_secs_f64();
            assert!(
                advance >= target * 0.5,
                "[{label}] PROD DRM stalled: advanced {advance:.2}s in {target:.2}s \
                 (pre={pre:.2}s, post={post:.2}s)"
            );
        }
        _ => {}
    }
}

/// PROD DRM scripted scenario: same `forward → backward → middle`
/// dance the user runs manually with `cargo run -p kithara-app`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_scripted() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::scripted_forward_back_end()).await;
}

/// PROD DRM "seek after long play" — directly reproduces the user's
/// manual observation: long playback on a prod DRM track, then drag
/// the playhead back, expect a hang or false-EOF.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_seek_after_long_play() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK,
        scenarios::seek_backward_after_long_play_repro(),
    )
    .await;
}

/// Same scenario on a second prod DRM track so the bug surfaces
/// independently of one track's particular byte layout.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_seek_after_long_play_alt_track() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK_ALT,
        scenarios::seek_backward_after_long_play_repro(),
    )
    .await;
}

/// PROD DRM near-end seek pin for Bug #7 on real HE-AAC v2 fragments.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_seek_near_end() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::seek_near_end_repro()).await;
}

/// PROD DRM seeded fuzz, seed 42. The random trajectory is exactly
/// what surfaces Bug #6 on the local fixtures; running it against
/// the real prod URL pins that we'd catch the same on production
/// when creds are available.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
async fn user_sim_prod_drm_random_seed_42() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::random_seed(42, 10)).await;
}

/// PROD DRM long play (30 s) then backward seek — mirrors the user's
/// manual GUI procedure: settle into the track for a real stretch,
/// then drag the slider back. Symptom user reports: position hangs
/// or false-EOF auto-advance.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_long_play_then_seek_backward() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::long_play_then_seek_backward()).await;
}

/// PROD DRM long play (30 s) then forward seek into unbuffered tail.
/// Bug #5 path with substantial accumulated state.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_long_play_then_seek_forward() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::long_play_then_seek_forward()).await;
}

/// PROD DRM seek storm — aggressive successive seeks, mimicking a
/// user dragging the slider repeatedly. Loader has to cancel and
/// restart fetches under the keyserver-signed flow.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_seek_storm() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::seek_storm()).await;
}

/// PROD DRM seek backward after natural EOF — pin for Bug #6 silent
/// hang variant. Walks the track to natural end, then jumps back.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
async fn user_sim_prod_drm_seek_backward_after_natural_eof() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK,
        scenarios::seek_backward_after_natural_eof_repro(),
    )
    .await;
}

/// PROD DRM seeded fuzz, seed 1337 — second seed to surface
/// trajectory-specific bugs that seed 42 might miss.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
async fn user_sim_prod_drm_random_seed_1337() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::random_seed(1337, 12)).await;
}

/// PROD DRM — Auto-ABR up-switch + seek burst. **THE** scenario for
/// the user's manual repro: bug only happens with Auto ABR enabled,
/// Manual works fine. Plays 15 s so the ABR throughput estimator
/// commits an `UpSwitch`, then bursts 4 seeks across the track. Per
/// the user's report each post-switch seek either reaches false-EOF
/// or hangs. Harness panics on either symptom.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
async fn user_sim_prod_drm_auto_abr_upswitch_then_seek_burst() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK,
        scenarios::auto_abr_upswitch_then_seek_burst(),
    )
    .await;
}

/// Same scenario but on a second prod DRM track. Pins that the bug
/// is not content-specific — same Auto ABR up-switch + seek pattern,
/// different segments + different mvhd metadata.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
async fn user_sim_prod_drm_auto_abr_upswitch_then_seek_burst_alt() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK_ALT,
        scenarios::auto_abr_upswitch_then_seek_burst(),
    )
    .await;
}

/// PROD DRM — race repro: seek IMMEDIATELY after Loaded, without
/// waiting for the demuxer to actually start producing samples.
/// Mirrors the user's UI flow: click track in list, drag slider
/// before audio kicks in. Every `seek anchor path: SeekOutOfRange`
/// in `app.log` has `epoch=1` (fresh track, first seek) so the
/// race must fire on the very first seek attempt.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn user_sim_prod_drm_seek_immediately_after_loaded() {
    run_prod_drm_scenario_no_warmup(PROD_DRM_TRACK, 0.95).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn user_sim_prod_drm_seek_immediately_after_loaded_mid() {
    run_prod_drm_scenario_no_warmup(PROD_DRM_TRACK, 0.50).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn user_sim_prod_drm_seek_immediately_after_loaded_low() {
    run_prod_drm_scenario_no_warmup(PROD_DRM_TRACK, 0.20).await;
}

/// PROD DRM — the bare contract test the user actually performs in
/// the GUI: track in queue, select it, IMMEDIATELY scrub the slider
/// while the engine is still ramping up. No duration wait, no
/// `wait_for_position_at_least`, no per-seek-landed wait. The track
/// must NOT auto-advance. Doesn't matter which underlying bug fires
/// (`SeekOutOfRange` + decoder corruption, recreate loop, `byte_shift`
/// mismatch, EOF conflation with decode error, etc.) — the contract
/// is "scrubbing a queued track stays on that track".
///
/// `wait_for_loaded` mirrors the GUI's "loading…" placeholder before
/// the track resource is constructed; in `app` the slider is dead
/// until that point. After `Loaded` we scrub with no further warmup.
// flash(false): prod-CDN e2e; raw tokio::spawn ticker + wall-clock scrub/settle windows.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
async fn user_sim_prod_drm_rapid_scrub_no_warmup_no_advance() {
    let prod = build_prod_ctx();
    let queue = prod_queue(&prod, Some(Duration::from_millis(10)));
    let q_for_tick = queue.control();
    let tick = tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let track0 = queue
        .append(prod_drm_spec(PROD_DRM_TRACK, &prod))
        .expect("append production DRM track 0");
    let track1 = queue
        .append(prod_drm_spec(PROD_DRM_TRACK_ALT, &prod))
        .expect("append production DRM track 1");

    use kithara_integration_tests::user_sim::harness::wait_for_loaded;
    wait_for_loaded(&queue, track0, Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| panic!("prod DRM load fail: {e}"));
    queue
        .select(track0, Transition::None)
        .expect("select prod DRM");

    let check_not_advanced = |label: &str| {
        let current = queue.current().map(|e| e.id);
        if let Some(id) = current
            && id != track0
        {
            panic!(
                "AUTO-ADVANCE [{label}]: queue.current flipped to {id:?} \
                 (track0={track0:?}, track1={track1:?})"
            );
        }
    };

    let scrub_targets = [5.0_f64, 30.0, 60.0, 15.0, 90.0, 45.0, 20.0, 75.0];
    for target in scrub_targets {
        let _ = queue.seek(target);
        check_not_advanced(&format!("after seek({target:.2}s)"));
        time::sleep(Duration::from_millis(120)).await;
        check_not_advanced(&format!("post-seek({target:.2}s)+120ms"));
    }

    time::sleep(Duration::from_secs(5)).await;
    check_not_advanced("after 5s settle");

    tick.abort();
    let _ = tick.await;
}

/// Like `run_prod_drm_scenario` but seeks AS SOON AS the queue reports
/// `Loaded` — no `wait_for_position_at_least` before the seek. This is
/// what catches the race: Queue knows duration from playlist but the
/// decoder hasn't parsed the init segment's mvhd yet, so seek targets
/// past the demuxer-known timestamp fail `OutOfRange`.
async fn run_prod_drm_scenario_no_warmup(url: &str, ratio: f64) {
    use kithara::play::SeekOutcome;
    let prod = build_prod_ctx();
    let queue = prod_queue(&prod, Some(Duration::from_millis(10)));
    let q_for_tick = queue.control();
    let tick = tokio::task::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue
        .append(prod_drm_spec(url, &prod))
        .expect("append no-warmup production DRM track");

    use kithara_integration_tests::user_sim::harness::wait_for_loaded;
    wait_for_loaded(&queue, track_id, Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| panic!("prod DRM load fail: {e}"));
    queue
        .select(track_id, Transition::None)
        .expect("select prod DRM");

    // Wait until duration is *known* (post-mvhd) — that's the contract
    // moment after which a user-issued seek can reasonably target a
    // ratio of the track. Before mvhd parsing `duration_seconds()`
    // returns `None`, which is the deliberate "unknown" signal. Without
    // this wait, the test would race the demuxer.
    let dur_deadline = kithara::platform::time::Instant::now() + Duration::from_secs(30);
    let duration = loop {
        if let Some(d) = queue.duration_seconds() {
            break d;
        }
        if kithara::platform::time::Instant::now() >= dur_deadline {
            panic!("duration never became known within 30 s after Loaded");
        }
        sleep(Duration::from_millis(50)).await;
    };
    let target = (duration * ratio).clamp(0.0, duration);
    let outcome = queue
        .seek(target)
        .unwrap_or_else(|e| panic!("queue.seek Err: {e}"));
    if let SeekOutcome::PastEof {
        duration: reported_dur,
        ..
    } = outcome
    {
        panic!(
            "PastEof for ratio={ratio:.2} target={target:.2}s \
             reported_dur={reported_dur:?} queue.duration={duration:.2}s"
        );
    }
    let started = kithara::platform::time::Instant::now();
    let budget = Duration::from_secs(15);
    let mut landed = false;
    while started.elapsed() < budget {
        if let Some(pos) = queue.position_seconds()
            && (pos - target).abs() <= 2.5
        {
            landed = true;
            break;
        }
        // Also fail if the track flipped (auto-advance on false EOF)
        if queue.current().map(|e| e.id) != Some(track_id) {
            panic!(
                "AUTO-ADVANCE: track flipped during seek (target={target:.2}s, pos={:?})",
                queue.position_seconds()
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        landed,
        "HANG: seek to {target:.2}s (ratio={ratio:.2}) never landed within {budget:?} \
         (pos={:?}, dur={duration:.2}s) — user-reported bug",
        queue.position_seconds()
    );

    // Brief play after to confirm we're not in a hung state.
    sleep(Duration::from_secs(2)).await;
    let post_seek_pos = queue.position_seconds().unwrap_or(0.0);
    assert!(
        post_seek_pos > target - 0.5,
        "POST-SEEK HANG: position regressed after seek (target={target:.2}s, \
         post-seek+2s={post_seek_pos:.2}s)"
    );

    tick.abort();
    let _ = tick.await;
}

/// Live prod-DRM tracks, all on `cdn-hls-slicer.zvuk.com` behind the same
/// `zvuk-prod` provider. Every one carries a HE-AAC v2 rendition beside two
/// AAC-LC ones, and eight of the ten add a FLAC fMP4 rendition (all but
/// `79829257_2` and `34487517_1`), so the multi-track scenario mixes codecs
/// the way the user's GUI playlist does.
///
/// The first four come from the `crates/kithara-app/app.yaml` playlist; its
/// other three (`173388194_1`, `50984034_1`, `171515249_1`) answer 502 and are
/// left out, because a test that cannot reach its media reports the catalogue
/// rather than the player. The rest are live tracks added to carry more of the
/// catalogue than that playlist alone can.
///
/// A track here has to run past 100 s: the near-end scenario seeks to 90 % and
/// then measures a full 10 s window of audio after it. Durations, summed from
/// `#EXTINF` on 2026-08-21: 169.10, 222.41, 480.16, 142.22, 211.33, 199.14,
/// 180.05, 386.20, 437.41, 276.59 s. `160830411_2` is the one live candidate
/// left out — at 85.88 s a 90 % seek leaves 8.59 s, less than the window.
///
/// A track id resolves through exactly one variant suffix, and which one it is
/// varies per track; the other suffixes answer 502.
const PROD_DRM_PLAYLIST: &[&str] = &[
    "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/79829257_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/59232754_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/34487517_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/149150095_3/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/24288695_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/150005977_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/116711682_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/133269928_2/master.m3u8",
];

/// Amplitude above which a sample counts as content. Shared by the
/// render loop and [`assert_audio_live`] so both agree on what "the
/// engine is producing audio" means.
const SILENCE_FLOOR: f32 = 1.0e-4;
/// How long the engine may make no observable progress — no audio, or
/// no handover — before the scenario calls it the stall it hunts. Named
/// after the `audio_worker_loop no progress for 10s` window in
/// `app.log`, and wide enough for the live CDN to answer a media
/// playlist read, a per-segment key signing and a segment fetch.
const ENGINE_PROGRESS_BUDGET: Duration = Duration::from_secs(10);
/// Pause between render blocks while the engine has nothing to hand
/// back, so the loop paces against the network instead of spinning.
const RENDER_POLL: Duration = Duration::from_millis(20);

/// Drive the offline session forward until it has captured
/// `target_frames` frames of actual audio, ticking the queue between
/// blocks so async lifecycle (load dispatch, ABR commits, EOF
/// detection) advances. Returns the interleaved PCM
/// (`stereo × target_frames` samples).
///
/// The offline renderer has no wall clock: `OfflineBackend::render`
/// always hands back a full buffer, zero-filled while the PCM ring is
/// dry, so an unpaced loop captures "10 s of audio" in milliseconds.
/// Against a live CDN that measures the round trip rather than the
/// audio worker — `Queue::seek` returns its outcome optimistically and
/// the RT thread drops what the feeder buffered, so a near-end seek is
/// always followed by a refill an unpaced loop would read as silence.
/// Silent blocks therefore pace the loop instead of counting toward the
/// window, and only silence lasting [`ENGINE_PROGRESS_BUDGET`] is the
/// stall this scenario hunts.
async fn render_audio_frames(
    queue: &OfflineQueue<AppPools>,
    target_frames: usize,
    label: &str,
) -> Vec<f32> {
    let channels = usize::from(queue.host().spec().channels);
    let block_frames = usize::try_from(queue.host().max_block_frames().get())
        .expect("offline render block fits usize");
    let mut pcm = Vec::with_capacity(target_frames * channels);
    let mut silent_since = None;
    while pcm.len() / channels < target_frames {
        let block = queue.render(block_frames);
        let _ = queue.tick();
        if block.iter().any(|s| s.abs() > SILENCE_FLOOR) {
            silent_since = None;
            pcm.extend_from_slice(&block);
            continue;
        }
        let since = *silent_since.get_or_insert_with(kithara::platform::time::Instant::now);
        assert!(
            since.elapsed() < ENGINE_PROGRESS_BUDGET,
            "{label}: no audio for {ENGINE_PROGRESS_BUDGET:?} — the audio worker stalled, or \
             no stream was ever started (captured {captured} of {target_frames} frames)",
            captured = pcm.len() / channels
        );
        sleep(RENDER_POLL).await;
    }
    pcm
}

/// Tick the queue and pull render blocks until the named track becomes
/// the current item with a known duration. Bounded by wall clock rather
/// than by an iteration count: a render block costs microseconds here
/// (see [`render_audio_frames`]), so counting blocks bounds nothing.
async fn wait_for_handover(
    queue: &OfflineQueue<AppPools>,
    track_id: kithara::events::TrackId,
    label: &str,
) {
    let block_frames = usize::try_from(queue.host().max_block_frames().get())
        .expect("offline render block fits usize");
    let started = kithara::platform::time::Instant::now();
    loop {
        let _ = queue.tick();
        let _ = queue.render(block_frames);
        if queue.current().map(|e| e.id) == Some(track_id)
            && queue.duration_seconds().is_some_and(|d| d > 0.0)
        {
            return;
        }
        assert!(
            started.elapsed() < ENGINE_PROGRESS_BUDGET,
            "{label}: track {track_id:?} never became current with known duration within \
             {ENGINE_PROGRESS_BUDGET:?}"
        );
        sleep(RENDER_POLL).await;
    }
}

/// Treat a stereo-interleaved PCM buffer as "live audio" if its RMS
/// exceeds a small threshold AND a sizeable fraction of samples are
/// non-trivial. [`render_audio_frames`] already refuses to count a
/// fully silent block, so this is the level check on top: a window
/// stitched from blocks that each carry one click and nothing else
/// does NOT pass.
fn assert_audio_live(samples: &[f32], label: &str) {
    assert!(
        !samples.is_empty(),
        "{label}: received zero PCM samples — engine never produced audio"
    );
    let mut sum_sq = 0.0_f64;
    let mut nonzero: u32 = 0;
    for &s in samples {
        let s_f = f64::from(s);
        sum_sq += s_f * s_f;
        if s.abs() > SILENCE_FLOOR {
            nonzero = nonzero.saturating_add(1);
        }
    }
    // Cap at u32::MAX so `f64::from(...)` is lossless. A 10 s buffer
    // at 44.1 kHz × stereo is ~882 k samples — far below u32::MAX.
    let total_samples = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let total = f64::from(total_samples);
    let rms = (sum_sq / total).sqrt();
    let nonzero_ratio = f64::from(nonzero) / total;
    assert!(
        rms >= 0.001 && nonzero_ratio >= 0.3,
        "{label}: silence detected — rms={rms:.5} non_zero_ratio={nonzero_ratio:.3} over \
         {} interleaved samples. The audio worker stalled (HangDetector either fired \
         or the PCM ring is dry).",
        samples.len()
    );
}

/// PROD plain HLS playlist sourced from `app.yaml`. Same provider
/// model as the DRM ladder (multi-variant ABR ladder, fMP4 init +
/// segments, no AES-128 keyserver). Used to isolate the DRM-specific
/// surface of the variant-switch recreate hang: if the hang fires
/// here too, the bug lives in `HlsVariant`/recreate, not the PKCS7
/// padding seam.

/// Body of both `user_sim_prod_*_multi_track_select_seek_end_hang`
/// tests. PCM-driven scenario: the test thread renders the audio graph
/// itself, so nothing advances unless the engine produces. For every
/// track we:
///   1. select,
///   2. capture 10 s of audio and assert it's *live* (non-trivial RMS),
///   3. seek near-end,
///   4. capture another 10 s and assert it's still live.
///
/// A stalled audio worker — whether it tripped the `HangDetector`
/// (which `panic!`s and aborts the process) or just stopped producing
/// PCM (PCM ring drains to silence) — fails inside
/// [`render_audio_frames`], which gives the engine
/// [`ENGINE_PROGRESS_BUDGET`] to hand back a block carrying content. A
/// position-progress heuristic is not enough: the cached
/// `position_seconds()` can advance even when the actual audio is
/// silence (the timeline commits the seek-landed position before any
/// chunk is decoded). Reading PCM is the ground truth.
async fn run_multi_track_select_seek_end_hang(urls: &[&str], label: &str) {
    use kithara::play::SeekOutcome;

    let prod = build_prod_ctx();
    let queue = prod_queue(&prod, None);
    let ten_seconds_frames = usize::try_from(queue.host().spec().sample_rate.get())
        .expect("offline sample rate fits usize")
        .checked_mul(10)
        .expect("ten-second render frame count fits usize");

    let mut track_ids = Vec::with_capacity(urls.len());
    for url in urls {
        track_ids.push(
            queue
                .append(prod_drm_spec(url, &prod))
                .expect("append multi-track production DRM track"),
        );
    }

    use kithara_integration_tests::user_sim::harness::wait_for_loaded;
    wait_for_loaded(&queue, track_ids[0], Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| panic!("{label}[0] load fail: {e}"));

    let rotations: u32 = if track_ids.len() >= 7 { 2 } else { 4 };
    for rotation in 0..rotations {
        for (idx, &track_id) in track_ids.iter().enumerate() {
            let ctx = format!("{label} [rot={rotation} idx={idx}]");

            queue
                .select(track_id, Transition::None)
                .unwrap_or_else(|e| panic!("{ctx} select Err: {e}"));

            wait_for_handover(&queue, track_id, &ctx).await;

            let phase1 = format!("{ctx} phase1 (post-select)");
            let pcm_phase1 = render_audio_frames(&queue, ten_seconds_frames, &phase1).await;
            assert_audio_live(&pcm_phase1, &phase1);

            let duration = queue
                .duration_seconds()
                .expect("duration known after wait_for_handover");
            let target = (duration * 0.90).clamp(0.0, duration);
            let outcome = queue
                .seek(target)
                .unwrap_or_else(|e| panic!("{ctx} seek Err: {e}"));
            assert!(
                matches!(outcome, SeekOutcome::Landed { .. }),
                "{ctx} seek to {target:.2}s of a {duration:.2}s track reported {outcome:?} — \
                 the reader parked at the end, so there is no post-seek audio to measure"
            );

            let phase2 = format!("{ctx} phase2 (post-near-end-seek)");
            let pcm_phase2 = render_audio_frames(&queue, ten_seconds_frames, &phase2).await;
            assert_audio_live(&pcm_phase2, &phase2);
        }
    }
}

/// PROD DRM multi-track near-end seek + ABR up-switch hang repro from
/// `app.log` (line 1849: `[HangDetector] audio_worker_loop no progress
/// for 10s`). Mirrors the user's manual GUI flow with prod DRM tracks
/// from `app.yaml`. Codec-agnostic: app.log captured the same hang on
/// `AacLc` and Flac variants on different runs.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn user_sim_prod_drm_multi_track_select_seek_end_hang() {
    run_multi_track_select_seek_end_hang(PROD_DRM_PLAYLIST, "prod-drm").await;
}
