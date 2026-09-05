#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::fmt::Write;

use kithara::{
    decode::DecoderBackend,
    events::AbrMode,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration, tokio},
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::{
        AudioCodec,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, fixture_protocol::EncryptionRequest, kithara,
    offline::OfflineQueue, temp_dir,
};
use kithara_test_fixtures::SignalAsset;
use url::Url;

use super::{
    actions::Action,
    harness::{SimHarness, TrackSpec},
    scenarios,
};
use crate::bufpool_ext::pools;

/// AES-128 key+IV pair shared across the integration suite. Mirrors
/// `track_replay_after_switch.rs::Consts::AES_KEY` and the
/// `local_track_plays.rs` encrypted fixtures.
const AES_KEY: &[u8] = b"0123456789abcdef";
const AES_IV: [u8; 16] = [0u8; 16];

const WARMUP: Duration = Duration::from_millis(500);

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut s, "{b:02x}").expect("hex write");
    }
    s
}

/// Matrix of track kinds the user-simulation harness exercises. Each
/// case lights up a different path through the player. Multi-variant
/// HLS kinds mirror the production `app.yaml` master playlists which
/// expose 3-4 quality levels (LQ AAC / MQ AAC / HQ AAC / lossless
/// FLAC) — driving ABR Auto from `enter_track` produces the same
/// up-switch cascade the user observes in the binary.
#[derive(Clone, Copy, Debug)]
enum TrackKind {
    /// Plain file pipeline, MP3 with extension in URL path.
    Mp3File,
    /// Plain file pipeline, extension-less URL (mirrors prod
    /// `cdn-edge.zvq.me/track/streamhq?id=*`).
    Mp3StreamHq,
    /// 4-variant AAC-LC master playlist with mixed bandwidths
    /// (1.28 Mb/s / 2.56 / 5.12 / 8 Mb/s). Same shape as the prod
    /// `low / mid / high / lossless` ladder minus the FLAC tail.
    HlsAacLcAbr4,
    /// 4-variant ladder with the top variant transcoded to FLAC,
    /// exactly like the production `master.m3u8` zvq.me ships.
    /// Forces the variant-switch path to handle a cross-codec move.
    HlsMixedCodecAbr4,
    /// Same 4-variant AAC-LC ladder under AES-128 — production DRM
    /// path. MANDATORY per the DRM-feedback memory.
    HlsAacLcDrmAbr4,
}

/// Build a track spec for `kind`. HLS fixtures get 16 segments ×
/// 4 s = 64 s of media so `SeekNearEnd` lands with room before
/// natural EOF.
async fn build_spec(
    helper: &TestServerHelper,
    kind: TrackKind,
    abr: AbrMode,
    backend: DecoderBackend,
) -> TrackSpec {
    let url = match kind {
        TrackKind::Mp3File => helper.signal(SignalAsset::MP3_SINE880_48K_162S),
        TrackKind::Mp3StreamHq => helper.streamhq(SignalAsset::MP3_SINE880_48K_162S),
        TrackKind::HlsAacLcAbr4 => build_hls_abr(helper, false, false).await,
        TrackKind::HlsMixedCodecAbr4 => build_hls_abr(helper, false, true).await,
        TrackKind::HlsAacLcDrmAbr4 => build_hls_abr(helper, true, false).await,
    };
    TrackSpec::new(url, backend)
        .with_abr_mode(abr)
        .with_backend(backend)
}

/// Production-shaped 4-variant AAC-LC ladder with optional DRM or FLAC tail.
async fn build_hls_abr(helper: &TestServerHelper, drm: bool, mixed_codec: bool) -> Url {
    let mut builder = HlsFixtureBuilder::new()
        .variant_count(4)
        .segments_per_variant(16)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000, 2_560_000, 5_120_000, 8_000_000])
        .packaged_audio_aac_lc(44_100, 2);
    if drm {
        builder = builder.encryption(EncryptionRequest {
            key_hex: hex_encode(AES_KEY),
            iv_hex: Some(hex_encode(&AES_IV)),
        });
    }
    if mixed_codec {
        builder = builder.override_variant_codec(3, AudioCodec::Flac);
    }
    helper
        .create_hls(builder)
        .await
        .expect("create 4-variant HLS fixture")
        .master_url()
}

async fn run_scenario(specs: Vec<TrackSpec>, actions: Vec<Action>) {
    let temp = temp_dir();
    let mut harness = SimHarness::new(temp.path(), &specs).await;
    harness.enter_track(0, WARMUP).await;
    for action in actions {
        let label = action.label();
        tracing::debug!(action = %label, "user_sim: applying");
        harness.apply(action).await;
    }
    harness.shutdown().await;
}

async fn run_single(kind: TrackKind, abr: AbrMode, actions: Vec<Action>) {
    run_single_backend(kind, abr, DecoderBackend::Symphonia, actions).await;
}

async fn run_single_backend(
    kind: TrackKind,
    abr: AbrMode,
    backend: DecoderBackend,
    actions: Vec<Action>,
) {
    let helper = TestServerHelper::new().await;
    let spec = build_spec(&helper, kind, abr, backend).await;
    run_scenario(vec![spec], actions).await;
}

/// Multi-track helper. Builds N `TrackSpecs` and appends them to the
/// same Queue so scenarios can `SelectAt(idx)` between them. ABR
/// mode is `Auto(None)` for every track (production default).
async fn run_multi(kinds: &[TrackKind], actions: Vec<Action>) {
    let helper = TestServerHelper::new().await;
    let mut specs = Vec::with_capacity(kinds.len());
    for kind in kinds {
        specs.push(
            build_spec(
                &helper,
                *kind,
                AbrMode::Auto(None),
                DecoderBackend::Symphonia,
            )
            .await,
        );
    }
    run_scenario(specs, actions).await;
}

// ─── Repro pins for bugs #5 / #6 / #7 ────────────────────────────────────────

/// Bug #5 — forward seek into the unbuffered tail triggers false EOF
/// and auto-advance. HLS cases are exercised under every ABR mode so
/// the same `seek_forward_unbuffered` path hits Auto's switch-decision
/// arm AND Manual's no-switch arm.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::mp3_streamhq(TrackKind::Mp3StreamHq, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_forward_unbuffered_repro(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::seek_forward_unbuffered_repro()).await;
}

/// Bug #6 — backward seek causes silent hang. `PlayFor` watchdog in
/// the harness panics on stuck position.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::mp3_streamhq(TrackKind::Mp3StreamHq, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_backward_repro(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::seek_backward_repro()).await;
}

/// Bug #7 — seek to 95-99 % crashes the decoder thread. With the
/// 64 s fixture, 97 % = 62.08 s leaves ~2 s before EOF.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::mp3_streamhq(TrackKind::Mp3StreamHq, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_near_end_repro(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::seek_near_end_repro()).await;
}

/// Production symptom: long playback → backward seek → silent hang
/// or false-EOF.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_backward_after_long_play(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::seek_backward_after_long_play_repro()).await;
}

/// Pinpoint: play to natural EOF, then seek backward.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_backward_after_natural_eof(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(
        kind,
        abr,
        scenarios::seek_backward_after_natural_eof_repro(),
    )
    .await;
}

// ─── Scripted "obligatory" scenario ──────────────────────────────────────────

/// Scripted scenario from the plan: 90 % → 10 % → 50 %. Each ABR
/// mode separately because Auto changes variant during the trajectory.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_scripted_forward_back_end(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::scripted_forward_back_end()).await;
}

// ─── Seeded random fuzz ──────────────────────────────────────────────────────

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(240)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_random_seed_42(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::random_seed(42, 12)).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(240)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_random_seed_1337(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::random_seed(1337, 12)).await;
}

// ─── Long-play scenarios ─────────────────────────────────────────────────────

/// 30 s playback then backward seek — the production manual repro.
/// HLS + DRM matrix; ABR Auto since that's the default users hit.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_long_play_then_seek_backward(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::long_play_then_seek_backward()).await;
}

/// 30 s playback then forward seek — Bug #5 path on long playback.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_long_play_then_seek_forward(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::long_play_then_seek_forward()).await;
}

/// Local repro for the "`PastEof` on fresh Loaded" race. The user's
/// production bug fires on the very first seek after a track changes
/// status to `Loaded`, before the demuxer has parsed the mvhd box.
/// `Queue::duration_seconds()` returns `Some(0.0)` in that window, and
/// `Player::seek_seconds` then evaluates `target_secs >= dur` as
/// `0 >= 0 == true` → `SeekOutcome::PastEof` → false-EOF auto-advance.
///
/// This test loads a multi-variant HLS DRM track (matching the
/// production playlist shape) and seeks the moment status flips to
/// `Loaded`, exactly like the prod UI does.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::aac_drm(TrackKind::HlsAacLcDrmAbr4, 0.50)]
#[case::aac_drm_low(TrackKind::HlsAacLcDrmAbr4, 0.20)]
#[case::aac_drm_high(TrackKind::HlsAacLcDrmAbr4, 0.95)]
#[case::aac_plain(TrackKind::HlsAacLcAbr4, 0.50)]
#[case::mp3_streamhq(TrackKind::Mp3StreamHq, 0.50)]
async fn user_sim_seek_immediately_after_loaded(#[case] kind: TrackKind, #[case] ratio: f64) {
    let helper = TestServerHelper::new().await;
    let spec = build_spec(
        &helper,
        kind,
        AbrMode::Auto(None),
        DecoderBackend::Symphonia,
    )
    .await;
    let temp = temp_dir();
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .build(),
    );
    let store = kithara_integration_tests::disk_asset_store(temp.path());
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let cfg = kithara::play::ResourceConfig::for_src(
        kithara::play::ResourceSrc::parse(spec.url.as_str()).expect("valid track URL"),
    )
    .worker(worker.clone())
    .downloader(downloader.clone())
    .store(store)
    .decoder(
        kithara::audio::AudioDecoderConfig::builder()
            .backend(DecoderBackend::Symphonia)
            .build(),
    )
    .initial_abr_mode(AbrMode::Auto(None))
    .build();
    let session_config = HostConfig::offline(pools)
        .pacing(Duration::from_millis(10))
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session_config.sample_rate())
            .worker(worker)
            .build(),
    );
    let queue = OfflineQueue::new(
        session_config,
        Queue::new(QueueConfig::builder().player(player).build()),
    )
    .expect("create product offline queue");
    let q_for_tick = queue.control();
    // Platform spawn chokepoint, NOT raw `tokio::spawn`: under flash
    // this makes the tick driver a quiescence participant with a
    // virtual `sleep`, so the virtual clock cannot race past the ticks
    // that drive loading. A raw spawn runs uncounted on real time.
    let tick = tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue
        .append(TrackSource::Config(Box::new(cfg)))
        .expect("append immediate-seek track");

    use super::harness::wait_for_loaded;
    wait_for_loaded(&queue, track_id, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("load fail: {e}"));
    queue
        .select(track_id, Transition::None)
        .expect("select track");

    // IMMEDIATELY (no warmup) seek — exactly like the user's UI click
    // right after the track turns "ready" in the playlist.
    let dur_at_seek = queue.duration_seconds().unwrap_or(0.0);
    let target = (dur_at_seek * ratio).clamp(0.0, dur_at_seek);
    let outcome = queue
        .seek(target)
        .unwrap_or_else(|e| panic!("queue.seek Err: {e}"));
    if let kithara::play::SeekOutcome::PastEof {
        duration: reported_dur,
        ..
    } = outcome
    {
        panic!(
            "FRESH-LOADED SEEK RACE BUG: Queue::seek returned PastEof for \
             ratio={ratio:.2} target={target:.2}s reported_dur={reported_dur:?} \
             queue.duration={dur_at_seek:.2}s — Loaded status fires before mvhd \
             is parsed; seek target lands at 0 → PastEof → false-EOF auto-advance"
        );
    }

    tick.abort();
    let _ = tick.await;
}

/// Aggressive seek storm — many seeks in rapid succession, like a
/// user dragging the slider. Loader has to cancel and restart fetches.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
async fn user_sim_seek_storm(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::seek_storm()).await;
}

/// **Auto-ABR up-switch + seek burst** — repro for the prod bug user
/// reports in `app.log`: after `commit_variant_switch reason=UpSwitch`
/// every subsequent seek returns `SeekOutOfRange` / false-EOF / hang.
/// Manual ABR (no switch) plays + seeks fine.
///
/// Parametrised over Auto (the bug path) AND Manual (the pin — must
/// always stay green). The Manual cases protect against accidentally
/// breaking the working path while fixing the Auto one.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
async fn user_sim_auto_abr_upswitch_then_seek_burst(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    run_single(kind, abr, scenarios::auto_abr_upswitch_then_seek_burst()).await;
}

// ─── Multi-track (DRM ↔ non-DRM) scenarios ──────────────────────────────────
// User reports the seek bug specifically on DRM. To stress the
// cleanup/re-init seam between encrypted and plain pipelines we
// queue mixed content and bounce between tracks. The matrix below
// covers each ordering of DRM/non-DRM combinations.

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::drm_then_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_then_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::mp3_then_drm(&[TrackKind::Mp3File, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
#[case::flac_then_drm(&[TrackKind::HlsMixedCodecAbr4, TrackKind::HlsAacLcDrmAbr4])]
async fn user_sim_switch_track_then_seek(#[case] kinds: &[TrackKind]) {
    run_multi(kinds, scenarios::switch_track_then_seek()).await;
}

/// Many `SelectAt` + seek bounces between two tracks. Lights up the
/// "previous DRM key state still mounted" path if there is one.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::drm_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::drm_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
async fn user_sim_bounce_between_tracks_with_seeks(#[case] kinds: &[TrackKind]) {
    run_multi(kinds, scenarios::bounce_between_tracks_with_seeks()).await;
}

/// Long play on first track, switch to next, seek inside immediately.
/// Mirrors the user's manual ride: settle into a track for a while,
/// then jump to another in the playlist and drag the playhead.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::drm_then_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_then_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::drm_then_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
async fn user_sim_long_play_then_switch_then_seek(#[case] kinds: &[TrackKind]) {
    run_multi(kinds, scenarios::long_play_then_switch_then_seek()).await;
}

/// Three-track DRM-heavy playlist: DRM → plain → DRM. The second
/// DRM track must initialise fresh — covers the per-track DRM key
/// state isolation path.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::drm_plain_drm(&[
    TrackKind::HlsAacLcDrmAbr4,
    TrackKind::HlsAacLcAbr4,
    TrackKind::HlsAacLcDrmAbr4,
])]
#[case::plain_drm_plain(&[
    TrackKind::HlsAacLcAbr4,
    TrackKind::HlsAacLcDrmAbr4,
    TrackKind::HlsAacLcAbr4,
])]
async fn user_sim_three_track_bounce_with_seeks(#[case] kinds: &[TrackKind]) {
    // Walk all three with seeks: 0 → seek mid → 1 → seek mid → 2 → seek mid.
    let actions = vec![
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_millis(800)),
        Action::SelectAt(1),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_millis(800)),
        Action::SelectAt(2),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_secs(2)),
    ];
    run_multi(kinds, actions).await;
}

// ─── Apple decoder backend (macOS/iOS) ─────────────────────────────────────
// Hardware-decoder path. Production users on Mac/iOS see the
// AudioToolbox path — repro the seek bugs on this backend too.
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple_backend {
    use super::*;

    #[::kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
    #[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
    #[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
    async fn user_sim_seek_storm_apple(#[case] kind: TrackKind, #[case] abr: AbrMode) {
        kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);
        run_single_backend(kind, abr, DecoderBackend::Apple, scenarios::seek_storm()).await;
    }

    #[::kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
    #[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
    #[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
    async fn user_sim_long_play_then_seek_backward_apple(
        #[case] kind: TrackKind,
        #[case] abr: AbrMode,
    ) {
        kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);
        run_single_backend(
            kind,
            abr,
            DecoderBackend::Apple,
            scenarios::long_play_then_seek_backward(),
        )
        .await;
    }
}
