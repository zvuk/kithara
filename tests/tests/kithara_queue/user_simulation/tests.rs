#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{fmt::Write, sync::Arc};

use kithara_app::{config::AppConfig, sources::build_source};
use kithara_assets::{FlushHub, FlushPolicy, StoreOptions};
use kithara_decode::DecoderBackend;
use kithara_events::AbrMode;
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, fixture_protocol::EncryptionRequest, kithara,
    offline::OfflineSession, temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep},
};
use kithara_play::{PlayerConfig, PlayerImpl};
use kithara_queue::{Queue, QueueConfig, TrackSource, Transition};
use kithara_stream::{
    AudioCodec,
    dl::{Downloader, DownloaderConfig},
};
use url::Url;

use super::{
    actions::Action,
    harness::{SimHarness, TrackSpec},
    scenarios,
};

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
        TrackKind::Mp3File => helper.asset("track.mp3"),
        TrackKind::Mp3StreamHq => helper.streamhq("track.mp3"),
        TrackKind::HlsAacLcAbr4 => build_hls_aac_abr(helper, false).await,
        TrackKind::HlsMixedCodecAbr4 => build_hls_mixed_codec_abr(helper).await,
        TrackKind::HlsAacLcDrmAbr4 => build_hls_aac_abr(helper, true).await,
    };
    TrackSpec::new(url, backend)
        .with_abr_mode(abr)
        .with_backend(backend)
}

/// Production-shaped 4-variant AAC-LC ladder, optional AES-128 DRM.
async fn build_hls_aac_abr(helper: &TestServerHelper, drm: bool) -> Url {
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
    helper
        .create_hls(builder)
        .await
        .expect("create 4-variant HLS fixture")
        .master_url()
}

/// 4-variant ladder where the top variant is FLAC — mirrors prod
/// `master.m3u8` (3 AAC + 1 FLAC lossless). Switching to variant 3
/// from any of 0-2 forces a cross-codec recreate; harness asserts
/// the codec snapshot survives the move (Bug #4-adjacent).
async fn build_hls_mixed_codec_abr(helper: &TestServerHelper) -> Url {
    let builder = HlsFixtureBuilder::new()
        .variant_count(4)
        .segments_per_variant(16)
        .segment_duration_secs(4.0)
        .variant_bandwidths(vec![1_280_000, 2_560_000, 5_120_000, 8_000_000])
        .packaged_audio_aac_lc(44_100, 2)
        .override_variant_codec(3, AudioCodec::Flac);
    helper
        .create_hls(builder)
        .await
        .expect("create mixed-codec ladder")
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

async fn run_single(
    helper: &TestServerHelper,
    kind: TrackKind,
    abr: AbrMode,
    actions: Vec<Action>,
) {
    run_single_backend(helper, kind, abr, DecoderBackend::Symphonia, actions).await;
}

async fn run_single_backend(
    helper: &TestServerHelper,
    kind: TrackKind,
    abr: AbrMode,
    backend: DecoderBackend,
    actions: Vec<Action>,
) {
    let spec = build_spec(helper, kind, abr, backend).await;
    run_scenario(vec![spec], actions).await;
}

/// Multi-track helper. Builds N `TrackSpecs` and appends them to the
/// same Queue so scenarios can `SelectAt(idx)` between them. ABR
/// mode is `Auto(None)` for every track (production default).
async fn run_multi(helper: &TestServerHelper, kinds: &[TrackKind], actions: Vec<Action>) {
    let mut specs = Vec::with_capacity(kinds.len());
    for kind in kinds {
        specs.push(
            build_spec(
                helper,
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
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(60)))]
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
    let helper = TestServerHelper::new().await;
    run_single(
        &helper,
        kind,
        abr,
        scenarios::seek_forward_unbuffered_repro(),
    )
    .await;
}

/// Bug #6 — backward seek causes silent hang. `PlayFor` watchdog in
/// the harness panics on stuck position.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(60)))]
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
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::seek_backward_repro()).await;
}

/// Bug #7 — seek to 95-99 % crashes the decoder thread. With the
/// 64 s fixture, 97 % = 62.08 s leaves ~2 s before EOF.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(60)))]
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
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::seek_near_end_repro()).await;
}

/// Production symptom: long playback → backward seek → silent hang
/// or false-EOF.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_backward_after_long_play(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(
        &helper,
        kind,
        abr,
        scenarios::seek_backward_after_long_play_repro(),
    )
    .await;
}

/// Pinpoint: play to natural EOF, then seek backward.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(90)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_seek_backward_after_natural_eof(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(
        &helper,
        kind,
        abr,
        scenarios::seek_backward_after_natural_eof_repro(),
    )
    .await;
}

// ─── Scripted "obligatory" scenario ──────────────────────────────────────────

/// Scripted scenario from the plan: 90 % → 10 % → 50 %. Each ABR
/// mode separately because Auto changes variant during the trajectory.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
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
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::scripted_forward_back_end()).await;
}

// ─── Seeded random fuzz ──────────────────────────────────────────────────────

#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(240)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_random_seed_42(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::random_seed(42, 12)).await;
}

#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(240)))]
#[case::mp3_file(TrackKind::Mp3File, AbrMode::Auto(None))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_random_seed_1337(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::random_seed(1337, 12)).await;
}

// ─── Long-play scenarios ─────────────────────────────────────────────────────

/// 30 s playback then backward seek — the production manual repro.
/// HLS + DRM matrix; ABR Auto since that's the default users hit.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_long_play_then_seek_backward(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(
        &helper,
        kind,
        abr,
        scenarios::long_play_then_seek_backward(),
    )
    .await;
}

/// 30 s playback then forward seek — Bug #5 path on long playback.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
async fn user_sim_long_play_then_seek_forward(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::long_play_then_seek_forward()).await;
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
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(60)))]
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
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let store = StoreOptions::new(temp.path());
    let cfg = kithara_play::ResourceConfig::for_src(spec.url.as_str())
        .expect("valid track URL")
        .downloader(downloader.clone())
        .store(store)
        .decoder_backend(DecoderBackend::Symphonia)
        .initial_abr_mode(AbrMode::Auto(None))
        .build();
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let q_for_tick = Arc::clone(&queue);
    let tick = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));

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
    if let kithara_play::SeekOutcome::PastEof {
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
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(60)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
async fn user_sim_seek_storm(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(&helper, kind, abr, scenarios::seek_storm()).await;
}

/// **Auto-ABR up-switch + seek burst** — repro for the prod bug user
/// reports in `app.log`: after `commit_variant_switch reason=UpSwitch`
/// every subsequent seek returns `SeekOutOfRange` / false-EOF / hang.
/// Manual ABR (no switch) plays + seeks fine.
///
/// Parametrised over Auto (the bug path) AND Manual (the pin — must
/// always stay green). The Manual cases protect against accidentally
/// breaking the working path while fixing the Auto one.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
#[case::aac_abr_manual_top(TrackKind::HlsAacLcAbr4, AbrMode::manual(3))]
#[case::aac_abr_manual0(TrackKind::HlsAacLcAbr4, AbrMode::manual(0))]
#[case::mixed_codec_auto(TrackKind::HlsMixedCodecAbr4, AbrMode::Auto(None))]
#[case::mixed_codec_manual_flac(TrackKind::HlsMixedCodecAbr4, AbrMode::manual(3))]
#[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
#[case::aac_drm_manual_top(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(3))]
#[case::aac_drm_manual0(TrackKind::HlsAacLcDrmAbr4, AbrMode::manual(0))]
async fn user_sim_auto_abr_upswitch_then_seek_burst(#[case] kind: TrackKind, #[case] abr: AbrMode) {
    let helper = TestServerHelper::new().await;
    run_single(
        &helper,
        kind,
        abr,
        scenarios::auto_abr_upswitch_then_seek_burst(),
    )
    .await;
}

// ─── Multi-track (DRM ↔ non-DRM) scenarios ──────────────────────────────────
// User reports the seek bug specifically on DRM. To stress the
// cleanup/re-init seam between encrypted and plain pipelines we
// queue mixed content and bounce between tracks. The matrix below
// covers each ordering of DRM/non-DRM combinations.

#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[case::drm_then_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_then_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::mp3_then_drm(&[TrackKind::Mp3File, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
#[case::flac_then_drm(&[TrackKind::HlsMixedCodecAbr4, TrackKind::HlsAacLcDrmAbr4])]
async fn user_sim_switch_track_then_seek(#[case] kinds: &[TrackKind]) {
    let helper = TestServerHelper::new().await;
    run_multi(&helper, kinds, scenarios::switch_track_then_seek()).await;
}

/// Many `SelectAt` + seek bounces between two tracks. Lights up the
/// "previous DRM key state still mounted" path if there is one.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::drm_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::drm_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
async fn user_sim_bounce_between_tracks_with_seeks(#[case] kinds: &[TrackKind]) {
    let helper = TestServerHelper::new().await;
    run_multi(
        &helper,
        kinds,
        scenarios::bounce_between_tracks_with_seeks(),
    )
    .await;
}

/// Long play on first track, switch to next, seek inside immediately.
/// Mirrors the user's manual ride: settle into a track for a while,
/// then jump to another in the playlist and drag the playhead.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[case::drm_then_plain(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsAacLcAbr4])]
#[case::plain_then_drm(&[TrackKind::HlsAacLcAbr4, TrackKind::HlsAacLcDrmAbr4])]
#[case::drm_then_mp3(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::Mp3File])]
#[case::drm_then_flac(&[TrackKind::HlsAacLcDrmAbr4, TrackKind::HlsMixedCodecAbr4])]
async fn user_sim_long_play_then_switch_then_seek(#[case] kinds: &[TrackKind]) {
    let helper = TestServerHelper::new().await;
    run_multi(&helper, kinds, scenarios::long_play_then_switch_then_seek()).await;
}

/// Three-track DRM-heavy playlist: DRM → plain → DRM. The second
/// DRM track must initialise fresh — covers the per-track DRM key
/// state isolation path.
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(180)))]
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
    let helper = TestServerHelper::new().await;
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
    run_multi(&helper, kinds, actions).await;
}

// ─── Apple decoder backend (macOS/iOS) ─────────────────────────────────────
// Hardware-decoder path. Production users on Mac/iOS see the
// AudioToolbox path — repro the seek bugs on this backend too.
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple_backend {
    use super::*;

    #[::kithara::test(tokio, multi_thread, timeout(Duration::from_secs(60)))]
    #[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
    #[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
    async fn user_sim_seek_storm_apple(#[case] kind: TrackKind, #[case] abr: AbrMode) {
        kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);
        let helper = TestServerHelper::new().await;
        run_single_backend(
            &helper,
            kind,
            abr,
            DecoderBackend::Apple,
            scenarios::seek_storm(),
        )
        .await;
    }

    #[::kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
    #[case::aac_abr_auto(TrackKind::HlsAacLcAbr4, AbrMode::Auto(None))]
    #[case::aac_drm_auto(TrackKind::HlsAacLcDrmAbr4, AbrMode::Auto(None))]
    async fn user_sim_long_play_then_seek_backward_apple(
        #[case] kind: TrackKind,
        #[case] abr: AbrMode,
    ) {
        kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);
        let helper = TestServerHelper::new().await;
        run_single_backend(
            &helper,
            kind,
            abr,
            DecoderBackend::Apple,
            scenarios::long_play_then_seek_backward(),
        )
        .await;
    }
}

// ─── Production DRM AAC v2 scenarios (#[ignore]-gated) ─────────────────────

/// Production zvuk DRM track URL — same one `zvuk_prod_drm_e2e.rs`
/// runs end-to-end. HE-AAC v2 fragments behind AES-128 + per-segment
/// X-Encrypted-Key signing; pinned because this is what the user
/// catches the seek bugs on manually.
const PROD_DRM_TRACK: &str = "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8";
/// Second prod DRM track — exercises the same provider but a
/// different track id, in case the bug is content-specific. URL
/// shape sourced from `app.yaml` playlist.
const PROD_DRM_TRACK_ALT: &str =
    "https://cdn-hls-slicer.zvuk.com/drm/track/173388194_1/master.m3u8";

/// Build a prod-DRM track via the same `kithara-app` source resolver
/// the binary uses. The resolver picks up baked credentials and the
/// `zvuk-prod` keyserver provider.
fn prod_drm_spec(url: &str, ctx: &ProdCtx) -> TrackSource {
    match build_source(url, &ctx.config) {
        TrackSource::Config(mut cfg) => {
            cfg.store = StoreOptions::new(ctx.cache.path());
            cfg.decoder_backend = DecoderBackend::Symphonia;
            cfg.initial_abr_mode = AbrMode::Auto(None);
            TrackSource::Config(cfg)
        }
        other => other,
    }
}

struct ProdCtx {
    config: AppConfig,
    cache: TestTempDir,
}

fn build_prod_ctx() -> ProdCtx {
    let net = NetOptions::builder().is_insecure(true).build();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancelToken::never())).build(),
    );
    let flush_hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
    let config = AppConfig::new(downloader, flush_hub, CancelToken::never());
    ProdCtx {
        config,
        cache: TestTempDir::new(),
    }
}

async fn run_prod_drm_scenario(url: &str, actions: Vec<Action>) {
    let prod = build_prod_ctx();
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let q_for_tick = Arc::clone(&queue);
    let tick = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue.append(prod_drm_spec(url, &prod));

    // Use the same harness assertions but skip the per-track-cache
    // bootstrap by driving the queue directly here — production
    // tracks are auth-gated so we need the `kithara-app` source
    // resolver, not `ResourceConfig::for_src`.
    use super::harness::{wait_for_loaded, wait_for_position_at_least};
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

async fn apply_action_to_queue(queue: &Arc<Queue>, action: &Action) {
    use kithara_play::SeekOutcome;
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
            let started = kithara_platform::time::Instant::now();
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
/// Requires baked production creds — gated by `#[ignore]`.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_scripted() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::scripted_forward_back_end()).await;
}

/// PROD DRM "seek after long play" — directly reproduces the user's
/// manual observation: long playback on a prod DRM track, then drag
/// the playhead back, expect a hang or false-EOF.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_seek_after_long_play_alt_track() {
    run_prod_drm_scenario(
        PROD_DRM_TRACK_ALT,
        scenarios::seek_backward_after_long_play_repro(),
    )
    .await;
}

/// PROD DRM near-end seek pin for Bug #7 on real HE-AAC v2 fragments.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_seek_near_end() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::seek_near_end_repro()).await;
}

/// PROD DRM seeded fuzz, seed 42. The random trajectory is exactly
/// what surfaces Bug #6 on the local fixtures; running it against
/// the real prod URL pins that we'd catch the same on production
/// when creds are available.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_random_seed_42() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::random_seed(42, 10)).await;
}

/// PROD DRM long play (30 s) then backward seek — mirrors the user's
/// manual GUI procedure: settle into the track for a real stretch,
/// then drag the slider back. Symptom user reports: position hangs
/// or false-EOF auto-advance.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_long_play_then_seek_backward() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::long_play_then_seek_backward()).await;
}

/// PROD DRM long play (30 s) then forward seek into unbuffered tail.
/// Bug #5 path with substantial accumulated state.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_long_play_then_seek_forward() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::long_play_then_seek_forward()).await;
}

/// PROD DRM seek storm — aggressive successive seeks, mimicking a
/// user dragging the slider repeatedly. Loader has to cancel and
/// restart fetches under the keyserver-signed flow.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_seek_storm() {
    run_prod_drm_scenario(PROD_DRM_TRACK, scenarios::seek_storm()).await;
}

/// PROD DRM seek backward after natural EOF — pin for Bug #6 silent
/// hang variant. Walks the track to natural end, then jumps back.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(300)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_seek_immediately_after_loaded() {
    run_prod_drm_scenario_no_warmup(PROD_DRM_TRACK, 0.95).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_seek_immediately_after_loaded_mid() {
    run_prod_drm_scenario_no_warmup(PROD_DRM_TRACK, 0.50).await;
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
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
#[kithara::test(flash(false), tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_rapid_scrub_no_warmup_no_advance() {
    let prod = build_prod_ctx();
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let q_for_tick = Arc::clone(&queue);
    let tick = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });

    let track0 = queue.append(prod_drm_spec(PROD_DRM_TRACK, &prod));
    let track1 = queue.append(prod_drm_spec(PROD_DRM_TRACK_ALT, &prod));

    use super::harness::wait_for_loaded;
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
        sleep(Duration::from_millis(120)).await;
        check_not_advanced(&format!("post-seek({target:.2}s)+120ms"));
    }

    sleep(Duration::from_secs(5)).await;
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
    use kithara_play::SeekOutcome;
    let prod = build_prod_ctx();
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));
    let q_for_tick = Arc::clone(&queue);
    let tick = tokio::spawn(async move {
        loop {
            sleep(Duration::from_millis(50)).await;
            if q_for_tick.tick().is_err() {
                break;
            }
        }
    });
    let track_id = queue.append(prod_drm_spec(url, &prod));

    use super::harness::wait_for_loaded;
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
    let dur_deadline = kithara_platform::time::Instant::now() + Duration::from_secs(30);
    let duration = loop {
        if let Some(d) = queue.duration_seconds() {
            break d;
        }
        if kithara_platform::time::Instant::now() >= dur_deadline {
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
    let started = kithara_platform::time::Instant::now();
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

/// Production DRM playlist sourced from `crates/kithara-app/app.yaml`.
/// All on `cdn-hls-slicer.zvuk.com` with the same `zvuk-prod` provider —
/// some are HE-AAC v2 fMP4, some FLAC fMP4, so the multi-track scenario
/// mixes codecs the way the user's GUI playlist does.
const PROD_DRM_PLAYLIST: &[&str] = &[
    "https://cdn-hls-slicer.zvuk.com/drm/track/173388194_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/50984034_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/79829257_2/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/171515249_1/master.m3u8",
    "https://cdn-hls-slicer.zvuk.com/drm/track/59232754_2/master.m3u8",
];

/// Default sample rate of `OfflineSession::new_manual()` — must
/// match `tests/src/offline/backend.rs::DEFAULT_SAMPLE_RATE`. Used
/// to convert "10 s of audio" into the frame count we need to render.
const OFFLINE_SAMPLE_RATE: usize = 44_100;
const STEREO_CHANNELS: usize = 2;
const TEN_SECONDS_FRAMES: usize = OFFLINE_SAMPLE_RATE * 10;
/// Per-`render()` request size. Matches the engine's typical block
/// — keeps render-loop wall-clock cost in the same ballpark as the
/// audio worker's tick.
const RENDER_BLOCK_FRAMES: usize = 1024;
/// Hard ceiling on render-loop iterations while waiting for the
/// next track to take over (handover). At 1024 frames / iteration
/// and 44.1 kHz this is ~4000 × 23ms = ~90 s, well above any
/// realistic prod CDN warmup. Translates a true hang into a clear
/// failure rather than letting the test run forever.
const HANDOVER_BLOCK_LIMIT: usize = 4000;

/// Drive the offline session forward by `target_frames` frames worth of
/// audio, ticking the queue between blocks so async lifecycle (load
/// dispatch, ABR commits, EOF detection) advances. Returns the captured
/// interleaved PCM (`stereo × target_frames` samples).
///
/// No `sleep` — wall-clock is driven entirely by `OfflineSession::render`
/// (which blocks on the engine's render dispatcher) and `Queue::tick`
/// (a single sync pass). If the audio worker is stalled the rendered
/// PCM will be silence, which the caller catches via `assert_audio_live`.
fn render_audio_frames(session: &OfflineSession, queue: &Queue, target_frames: usize) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(target_frames * STEREO_CHANNELS);
    let mut empty_blocks = 0usize;
    while pcm.len() / STEREO_CHANNELS < target_frames {
        let block = session.render(RENDER_BLOCK_FRAMES);
        let _ = queue.tick();
        if block.is_empty() {
            empty_blocks = empty_blocks.saturating_add(1);
            assert!(
                empty_blocks < HANDOVER_BLOCK_LIMIT,
                "OfflineSession::render returned empty for {empty_blocks} consecutive blocks — \
                 engine never started a stream (no player session or session worker dead)"
            );
            continue;
        }
        empty_blocks = 0;
        pcm.extend_from_slice(&block);
    }
    pcm
}

/// Tick the queue and pull empty render blocks until the named track
/// becomes the current item with a known duration. No `sleep`: the
/// render dispatcher provides the wall-clock cadence. Panics on
/// exceeding `HANDOVER_BLOCK_LIMIT` so a true handover failure surfaces
/// as a hard error instead of an infinite loop.
fn wait_for_handover(
    session: &OfflineSession,
    queue: &Queue,
    track_id: kithara_events::TrackId,
    label: &str,
) {
    for attempt in 0..HANDOVER_BLOCK_LIMIT {
        let _ = queue.tick();
        let _ = session.render(RENDER_BLOCK_FRAMES);
        if queue.current().map(|e| e.id) == Some(track_id)
            && queue.duration_seconds().is_some_and(|d| d > 0.0)
        {
            return;
        }
        let _ = attempt;
    }
    panic!(
        "{label}: track {track_id:?} never became current with known duration after \
         {HANDOVER_BLOCK_LIMIT} render blocks"
    );
}

/// Treat a stereo-interleaved PCM buffer as "live audio" if its RMS
/// exceeds a small threshold AND a sizeable fraction of samples are
/// non-trivial. A hung worker emits silence (zeros); a stalled-then-
/// recovered worker emits a brief silent prefix followed by content.
/// We require both metrics to be high so a buffer that's 90 % silence
/// + 10 % click does NOT pass.
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
        if s.abs() > 1.0e-4 {
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
const PROD_PLAIN_PLAYLIST: &[&str] = &[
    "https://stream.silvercomet.top/hls/master.m3u8",
    "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
];

/// Body of both `user_sim_prod_*_multi_track_select_seek_end_hang`
/// tests. PCM-driven scenario, no `sleep`: the test thread renders the
/// audio graph synchronously, so wall-clock advances only through real
/// engine work. For every track we:
///   1. select,
///   2. render 10 s of audio and assert it's *live* (non-trivial RMS),
///   3. seek near-end,
///   4. render another 10 s and assert it's still live.
///
/// A stalled audio worker — whether it tripped the `HangDetector`
/// (which `panic!`s and aborts the process) or just stopped producing
/// PCM (PCM ring drains to silence) — fails the assertion. A position-
/// progress heuristic is not enough: the cached `position_seconds()`
/// can advance even when the actual audio is silence (the timeline
/// commits the seek-landed position before any chunk is decoded).
/// Reading PCM is the ground truth.
async fn run_multi_track_select_seek_end_hang(urls: &[&str], label: &str) {
    use kithara_play::SessionDispatcher;

    let prod = build_prod_ctx();
    let session = Arc::new(OfflineSession::new_manual());
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .session(Arc::clone(&session) as Arc<dyn SessionDispatcher>)
            .build(),
    ));
    let queue = Arc::new(Queue::new(QueueConfig::default().with_player(player)));

    let mut track_ids = Vec::with_capacity(urls.len());
    for url in urls {
        track_ids.push(queue.append(prod_drm_spec(url, &prod)));
    }

    use super::harness::wait_for_loaded;
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

            wait_for_handover(&session, &queue, track_id, &ctx);

            let pcm_phase1 = render_audio_frames(&session, &queue, TEN_SECONDS_FRAMES);
            assert_audio_live(&pcm_phase1, &format!("{ctx} phase1 (post-select)"));

            let duration = queue
                .duration_seconds()
                .expect("duration known after wait_for_handover");
            let target = (duration * 0.90).clamp(0.0, duration);
            queue
                .seek(target)
                .unwrap_or_else(|e| panic!("{ctx} seek Err: {e}"));

            let pcm_phase2 = render_audio_frames(&session, &queue, TEN_SECONDS_FRAMES);
            assert_audio_live(&pcm_phase2, &format!("{ctx} phase2 (post-near-end-seek)"));
        }
    }
}

/// PROD DRM multi-track near-end seek + ABR up-switch hang repro from
/// `app.log` (line 1849: `[HangDetector] audio_worker_loop no progress
/// for 10s`). Mirrors the user's manual GUI flow with prod DRM tracks
/// from `app.yaml`. Codec-agnostic: app.log captured the same hang on
/// `AacLc` and Flac variants on different runs.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[ignore = "requires zvuk prod creds + cdn-hls-slicer.zvuk.com reachable"]
async fn user_sim_prod_drm_multi_track_select_seek_end_hang() {
    run_multi_track_select_seek_end_hang(PROD_DRM_PLAYLIST, "prod-drm").await;
}

/// Companion to the DRM variant on plain (non-encrypted) HLS tracks
/// from `app.yaml`. Isolation pin: if this fails too, the bug lives
/// in the generic variant-switch recreate path (`HlsVariant` /
/// `step_recreating_decoder`), not in the DRM PKCS7 `byte_shift` seam.
/// If this passes while the DRM variant fails, the bug is DRM-only.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
#[ignore = "requires plain HLS prod URLs reachable (stream.silvercomet.top + ecs-stage-slicer-01)"]
async fn user_sim_prod_plain_hls_multi_track_select_seek_end_hang() {
    run_multi_track_select_seek_end_hang(PROD_PLAIN_PLAYLIST, "prod-plain-hls").await;
}
