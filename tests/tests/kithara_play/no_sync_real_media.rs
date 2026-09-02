#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

#[path = "no_sync_real_media/oracle.rs"]
mod oracle;
#[path = "no_sync_real_media/reference.rs"]
mod reference;
#[path = "no_sync_real_media/runtime.rs"]
mod runtime;

use std::{num::NonZeroU32, path::PathBuf};

#[cfg(feature = "perf")]
use hotpath::HotpathGuardBuilder;
use kithara::{
    events::{EventBus, TrackId},
    hls::AbrMode,
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    play::{
        Cmd, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, Reply, Resource,
        ResourceConfig, ResourceSrc, SeekOutcome, SelectTransition, SessionDispatcher, apply_mix,
    },
    warp::{StretchControls, StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, audio_artifact::write_audio_artifact,
    cochlea::CochleaReport, fixture_protocol::PackagedSignal, memory_asset_store,
    offline::OfflineSession,
};
use kithara_test_fixtures::{SignalAsset, assets::by_name};
use oracle::{AudioLevelReport, AudioRole, MatchedMixReport, SampleContinuityReport};
use reference::capture_references;
use runtime::{Deck, DeckObservation, EventPolicy};
use serde::Serialize;
use url::Url;

use crate::bufpool_ext::{TestPools, pools};

const CHANNELS: u16 = 2;
const SOURCE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 128;
const CAPTURE_SECS: u32 = 2;
const CAPTURE_START_SECS: f64 = 10.0;
const CAPTURE_START_STEP_SECS: f64 = 4.0;
const MAX_SEEK_SECS: u32 = 8;
const CONTROL_SETTLE_BLOCKS: usize = 4;
const MIX_HEADROOM: f32 = 0.5;
const MIN_FIXED_STEM_RMS_DBFS: f64 = -50.0;
const MIN_DECK_CONTRIBUTION_RATIO: f64 = 0.02;
const MAX_DECK_GAIN_DELTA: f64 = 0.02;
const MAX_MATCHED_RMS_DELTA_DB: f64 = 0.1;
const POSITION_TOLERANCE_SECS: f64 = 0.15;
const SEEK_POSITION_TOLERANCE_SECS: f64 = 513.0 / 44_100.0;
const EXACT_ZERO_RUN_LIMIT_FRAMES: usize = 8;
const MIN_BOUNDARY_JUMP: f32 = 0.05;
const BOUNDARY_OUTLIER_RATIO: f32 = 6.0;
const PRELOAD_TIMEOUT: Duration = Duration::from_secs(30);
const HLS_LADDER_SEGMENTS: usize = 12;
const HLS_LADDER_SEGMENT_SECS: f64 = 4.0;
const HLS_SWEEP_START_HZ: f64 = 1_000.0;
const HLS_SWEEP_END_HZ: f64 = 5_000.0;

#[derive(Clone, Copy)]
enum Media {
    Mp3(SignalAsset),
    Hls,
}

impl Media {
    const fn label(self) -> &'static str {
        match self {
            Self::Mp3(asset) => asset.name(),
            Self::Hls => "hls-sweep",
        }
    }
}

struct Case {
    label: &'static str,
    host_rate: u32,
    media: &'static [Media],
}

const MP3_ONE: &[Media] = &[Media::Mp3(SignalAsset::MP3_TRACK_SINE440_187S)];
const MP3_TWO: &[Media] = &[
    Media::Mp3(SignalAsset::MP3_TRACK_SINE440_187S),
    Media::Mp3(SignalAsset::MP3_SINE880_48K_162S),
];
// Alternating media put two decks in the same body, `CAPTURE_START_STEP_SECS`
// apart. The mix oracle solves for one gain per deck, which needs their stems
// independent, so a body read twice has to read differently at the two
// offsets: chirps, not steady tones.
const MP3_FOUR: &[Media] = &[
    Media::Mp3(SignalAsset::MP3_SWEEP_UP_60S),
    Media::Mp3(SignalAsset::MP3_SWEEP_DOWN_60S),
    Media::Mp3(SignalAsset::MP3_SWEEP_UP_60S),
    Media::Mp3(SignalAsset::MP3_SWEEP_DOWN_60S),
];
const HLS_ONE: &[Media] = &[Media::Hls];
const HLS_MP3_TWO: &[Media] = &[Media::Hls, Media::Mp3(SignalAsset::MP3_SINE880_48K_162S)];
const HLS_MP3_FOUR: &[Media] = &[
    Media::Hls,
    Media::Mp3(SignalAsset::MP3_SINE880_48K_162S),
    Media::Hls,
    Media::Mp3(SignalAsset::MP3_TRACK_SINE440_187S),
];

const CASES: &[Case] = &[
    Case {
        label: "no-sync-mp3-one-44100",
        host_rate: 44_100,
        media: MP3_ONE,
    },
    Case {
        label: "no-sync-mp3-distinct-two-48000",
        host_rate: 48_000,
        media: MP3_TWO,
    },
    Case {
        label: "no-sync-mp3-alternating-four-44100",
        host_rate: 44_100,
        media: MP3_FOUR,
    },
    Case {
        label: "no-sync-hls-one-48000",
        host_rate: 48_000,
        media: HLS_ONE,
    },
    Case {
        label: "no-sync-hls-mp3-distinct-two-44100",
        host_rate: 44_100,
        media: HLS_MP3_TWO,
    },
    Case {
        label: "no-sync-hls-mp3-alternating-four-48000",
        host_rate: 48_000,
        media: HLS_MP3_FOUR,
    },
];

#[derive(Serialize)]
struct ArtifactManifest<'a> {
    case: &'a str,
    media: Vec<&'static str>,
    deck_count: usize,
    host_sample_rate: u32,
    channels: u16,
    requested_frames: usize,
    captured_frames: usize,
    capture_start_positions_secs: &'a [f64],
    reference_path: &'static str,
    direct_reference_gain: f32,
    runtime_deck_gain: f32,
    mix_tap_drops: u64,
    mix_tap_matches_output: bool,
    sample_continuity: Option<&'a SampleContinuityReport>,
    cochlea: Option<&'a CochleaReport>,
    audio_levels: &'a [AudioLevelReport],
    matched_mix: Option<&'a MatchedMixReport>,
    decks: &'a [DeckObservation],
    failures: &'a [String],
}

struct CapturedAudio {
    label: String,
    pcm: Vec<f32>,
    requested_frames: usize,
    tap_drops: u64,
    tap_matches_output: bool,
    start_positions_secs: Vec<f64>,
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
async fn no_sync_real_media_matrix_is_continuous_and_unsynchronized() {
    run_real_media_matrix(false).await;
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(360))
)]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_real_media_artifacts() {
    run_real_media_matrix(true).await;
}

/// The one body every HLS deck reads.
///
/// It sweeps instead of holding a tone because `HLS_MP3_FOUR` puts two decks
/// in this body `CAPTURE_START_STEP_SECS` apart, and the mix oracle solves a
/// least-squares system over the deck stems: two windows of the same steady
/// tone are near-collinear and leave that system singular. The sweep runs
/// continuously across segments, so the two windows land in different bands,
/// and its range clears the 440 Hz and 880 Hz tones the MP3 decks carry.
async fn hls_ladder_url(server: &TestServerHelper) -> Url {
    let deepest_capture = CAPTURE_START_SECS
        + (CASES
            .iter()
            .map(|case| case.media.len())
            .max()
            .expect("the matrix has cases") as f64
            - 1.0)
            * CAPTURE_START_STEP_SECS
        + f64::from(CAPTURE_SECS);
    let ladder_secs = HLS_LADDER_SEGMENTS as f64 * HLS_LADDER_SEGMENT_SECS;
    assert!(
        ladder_secs > deepest_capture,
        "the last deck captures through {deepest_capture} s but the ladder is only {ladder_secs} s",
    );

    server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(HLS_LADDER_SEGMENTS)
                .segment_duration_secs(HLS_LADDER_SEGMENT_SECS)
                .variant_bandwidths(vec![128_000])
                .packaged_audio_signal_aac_lc(
                    SOURCE_RATE,
                    CHANNELS,
                    PackagedSignal::Sweep {
                        start_hz: HLS_SWEEP_START_HZ,
                        end_hz: HLS_SWEEP_END_HZ,
                    },
                ),
        )
        .await
        .expect("create the ladder the HLS decks read")
        .master_url()
}

async fn run_real_media_matrix(record_artifacts: bool) {
    let server = TestServerHelper::new().await;
    let hls = hls_ladder_url(&server).await;
    let mut failures = Vec::new();
    for case in CASES {
        failures.extend(
            run_case(case, &hls, record_artifacts)
                .await
                .into_iter()
                .map(|failure| format!("{}: {failure}", case.label)),
        );
    }
    assert!(
        failures.is_empty(),
        "no-SYNC real-media matrix failed:\n{}",
        failures.join("\n"),
    );
}

async fn run_case(case: &Case, hls: &Url, record_artifacts: bool) -> Vec<String> {
    #[cfg(feature = "perf")]
    let _guard = HotpathGuardBuilder::new(case.label).build();

    let session = Arc::new(OfflineSession::new_manual_with_block_frames(BLOCK_FRAMES));
    let mut failures = Vec::new();

    let media_dir = TestTempDir::new();
    let mut decks = Vec::with_capacity(case.media.len());
    for (deck_index, media) in case.media.iter().copied().enumerate() {
        decks.push(prepare_deck(case, deck_index, media, hls, &media_dir, &session).await);
    }

    load_decks(case, &decks, &mut failures);
    runtime::record_transport_state(&session, "before first render", &mut failures);
    runtime::drain_all_events(
        &mut decks,
        "startup",
        EventPolicy::AudiblePlayback,
        &mut failures,
    );

    let deck_count = u16::try_from(decks.len()).expect("matrix deck count fits u16");
    let mix_level = MIX_HEADROOM / f32::from(deck_count);
    let mix_levels = vec![mix_level; decks.len()];
    let final_mix = capture_pass(
        case,
        &session,
        &mut decks,
        "final-mix",
        &mix_levels,
        &mut failures,
    )
    .await;
    let direct_references = capture_references(case, &mut decks, &final_mix, &mut failures).await;
    let direct_reference_pcm = direct_references
        .iter()
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    let matched_mix = oracle::assess_matched_mix(
        case.label,
        &final_mix.pcm,
        &direct_reference_pcm,
        &mut failures,
    );

    for (deck_index, deck) in decks.iter().enumerate() {
        runtime::validate_deck(case, deck_index, deck, &mut failures);
        runtime::record_control_state(case, deck_index, deck, "after capture", &mut failures);
    }
    runtime::record_transport_state(&session, "after capture", &mut failures);
    let oracles = oracle::assess_audio(case.label, case.host_rate, &final_mix.pcm, &mut failures);

    let mut audio_levels = direct_references
        .iter()
        .enumerate()
        .map(|(deck_index, reference)| {
            oracle::measure_audio_level(
                &format!("direct-reference-{deck_index}"),
                AudioRole::DirectReference,
                case.host_rate,
                reference,
            )
        })
        .collect::<Vec<_>>();
    audio_levels.extend(matched_mix.contributions.iter().enumerate().map(
        |(deck_index, contribution)| {
            oracle::measure_audio_level(
                &format!("contribution-{deck_index}"),
                AudioRole::Contribution,
                case.host_rate,
                contribution,
            )
        },
    ));
    audio_levels.push(oracle::measure_audio_level(
        "reference-mix",
        AudioRole::ReferenceMix,
        case.host_rate,
        &matched_mix.reference,
    ));
    audio_levels.push(oracle::measure_audio_level(
        &final_mix.label,
        AudioRole::FinalMix,
        case.host_rate,
        &final_mix.pcm,
    ));
    oracle::assess_listening_levels(case.label, &audio_levels, &mut failures);

    let observations: Vec<DeckObservation> =
        decks.into_iter().map(|deck| deck.observation).collect();
    let manifest = ArtifactManifest {
        case: case.label,
        media: case.media.iter().map(|media| media.label()).collect(),
        deck_count: case.media.len(),
        host_sample_rate: case.host_rate,
        channels: CHANNELS,
        requested_frames: final_mix.requested_frames,
        captured_frames: final_mix.pcm.len() / usize::from(CHANNELS),
        capture_start_positions_secs: &final_mix.start_positions_secs,
        reference_path: "independent resource decoder and host resampler",
        direct_reference_gain: 1.0,
        runtime_deck_gain: mix_level,
        mix_tap_drops: final_mix.tap_drops,
        mix_tap_matches_output: final_mix.tap_matches_output,
        sample_continuity: oracles.sample_continuity.as_ref(),
        cochlea: oracles.cochlea.as_ref(),
        audio_levels: &audio_levels,
        matched_mix: matched_mix.report.as_ref(),
        decks: &observations,
        failures: &failures,
    };
    if record_artifacts {
        let mut audio = direct_references
            .iter()
            .enumerate()
            .map(|(deck_index, reference)| {
                (
                    format!("direct-reference-{deck_index}"),
                    reference.as_slice(),
                )
            })
            .collect::<Vec<_>>();
        audio.extend(matched_mix.contributions.iter().enumerate().map(
            |(deck_index, contribution)| {
                (
                    format!("contribution-{deck_index}"),
                    contribution.as_slice(),
                )
            },
        ));
        audio.push(("reference-mix".to_owned(), &matched_mix.reference));
        audio.push((final_mix.label.clone(), &final_mix.pcm));
        let audio_slices = audio
            .iter()
            .map(|(label, pcm)| (label.as_str(), *pcm))
            .collect::<Vec<_>>();
        let written = write_audio_artifact(
            case.label,
            case.host_rate,
            CHANNELS,
            &audio_slices,
            &manifest,
        )
        .unwrap_or_else(|error| panic!("{}: write audio artifact: {error}", case.label));
        assert!(
            written.is_some(),
            "KITHARA_AUDIO_ARTIFACT_DIR must be set for the artifact recorder"
        );
    }
    failures
}

async fn capture_pass(
    case: &Case,
    session: &OfflineSession,
    decks: &mut [Deck],
    label: &str,
    levels: &[f32],
    failures: &mut Vec<String>,
) -> CapturedAudio {
    let ready = reset_for_capture(case, session, decks, label, levels, failures).await;
    let capture_blocks = oracle::blocks_for_secs(case.host_rate, CAPTURE_SECS);
    let requested_frames = capture_blocks * BLOCK_FRAMES;
    if !ready {
        return CapturedAudio {
            label: label.to_owned(),
            pcm: Vec::new(),
            requested_frames,
            tap_drops: 0,
            tap_matches_output: false,
            start_positions_secs: Vec::new(),
        };
    }

    let mut tap = session
        .enable_mix_tap(requested_frames * usize::from(CHANNELS) + BLOCK_FRAMES)
        .unwrap_or_else(|error| panic!("{} {label}: enable mix tap: {error}", case.label));
    let positions_before = decks
        .iter()
        .map(|deck| deck.player.position_seconds().unwrap_or(0.0))
        .collect::<Vec<_>>();
    let mut pcm = Vec::with_capacity(requested_frames * usize::from(CHANNELS));
    let mut zero_blocks = Vec::new();
    for block_index in 0..capture_blocks {
        let block = render_paced(session, decks, case.host_rate).await;
        inspect_block(
            case.label,
            label,
            block_index,
            &block,
            &mut zero_blocks,
            failures,
        );
        pcm.extend_from_slice(&block);
        runtime::drain_all_events(decks, label, EventPolicy::AudiblePlayback, failures);
    }
    for deck in &*decks {
        deck.player.process_notifications();
    }
    runtime::drain_all_events(
        decks,
        &format!("{label} final drain"),
        EventPolicy::AudiblePlayback,
        failures,
    );

    let tapped = tap.drain();
    let tap_drops = tap.drops();
    let tap_matches_output = assess_capture(
        case.label,
        label,
        &pcm,
        &tapped,
        tap_drops,
        requested_frames,
        &zero_blocks,
        failures,
    );
    disable_mix_tap(case.label, label, session, failures);
    assess_position_advance(
        case,
        label,
        decks,
        &positions_before,
        requested_frames,
        failures,
    );

    CapturedAudio {
        label: label.to_owned(),
        pcm,
        requested_frames,
        tap_drops,
        tap_matches_output,
        start_positions_secs: positions_before,
    }
}

async fn reset_for_capture(
    case: &Case,
    session: &OfflineSession,
    decks: &mut [Deck],
    label: &str,
    levels: &[f32],
    failures: &mut Vec<String>,
) -> bool {
    if levels.len() != decks.len() {
        failures.push(format!(
            "{} {label}: {} levels for {} decks",
            case.label,
            levels.len(),
            decks.len(),
        ));
        return false;
    }

    for deck in &*decks {
        deck.player.pause();
    }
    settle_controls(
        case,
        session,
        decks,
        "pause",
        EventPolicy::AudiblePlayback,
        failures,
    )
    .await;
    if let Err(error) = apply_mix(decks.iter().map(|deck| (deck.player.as_ref(), 0.0))) {
        failures.push(format!(
            "{} {label}: mute before seek failed: {error}",
            case.label,
        ));
        return false;
    }
    settle_controls(
        case,
        session,
        decks,
        "mute",
        EventPolicy::AudiblePlayback,
        failures,
    )
    .await;

    for deck in &mut *decks {
        deck.seek_request_epoch = None;
        deck.seek_complete_epoch = None;
        deck.muted_seek_underrun_epoch = None;
        deck.seek_terminal = false;
    }
    let mut requested = true;
    for (deck_index, deck) in decks.iter().enumerate() {
        match deck.player.seek_seconds(deck.capture_target_secs) {
            Ok(SeekOutcome::Landed { .. }) => {}
            Ok(SeekOutcome::PastEof { duration, .. }) => {
                requested = false;
                failures.push(format!(
                    "{} {label} deck {deck_index} ({}): capture start {:.3}s is past EOF at {:.3}s",
                    case.label,
                    deck.observation.label,
                    deck.capture_target_secs,
                    duration.as_secs_f64(),
                ));
            }
            Err(error) => {
                requested = false;
                failures.push(format!(
                    "{} {label} deck {deck_index} ({}): seek failed: {error}",
                    case.label, deck.observation.label,
                ));
            }
        }
    }
    if !requested {
        return false;
    }
    runtime::drain_all_events(
        decks,
        &format!("{label} seek request"),
        EventPolicy::AudiblePlayback,
        failures,
    );
    if decks.iter().any(|deck| deck.seek_request_epoch.is_none()) {
        failures.push(format!(
            "{} {label}: seek request epochs were not observed for every deck: {:?}",
            case.label,
            decks
                .iter()
                .map(|deck| deck.seek_request_epoch)
                .collect::<Vec<_>>(),
        ));
        return false;
    }

    for deck in &*decks {
        deck.player.play();
    }
    let mut completed = false;
    let mut seek_blocks = 0_u32;
    for _ in 0..oracle::blocks_for_secs(case.host_rate, MAX_SEEK_SECS) {
        let block = render_paced(session, decks, case.host_rate).await;
        seek_blocks += 1;
        if block.len() != BLOCK_FRAMES * usize::from(CHANNELS) {
            failures.push(format!(
                "{} {label}: seek render produced {} samples",
                case.label,
                block.len(),
            ));
        }
        runtime::drain_all_events(
            decks,
            &format!("{label} seek"),
            EventPolicy::MutedSeekSetup,
            failures,
        );
        for deck in &*decks {
            if deck.seek_complete_epoch == deck.seek_request_epoch
                && deck.muted_seek_underrun_epoch.is_none()
                && deck.player.is_playing()
            {
                deck.player.pause();
            }
        }
        if decks.iter().any(|deck| deck.seek_terminal) {
            break;
        }
        if decks.iter().all(|deck| {
            deck.seek_complete_epoch == deck.seek_request_epoch
                && deck.muted_seek_underrun_epoch.is_none()
        }) {
            completed = true;
            break;
        }
    }
    for deck in &*decks {
        deck.player.pause();
    }
    if !completed {
        failures.push(format!(
            "{} {label}: not every deck committed seek output within {MAX_SEEK_SECS}s; completions={:?}",
            case.label,
            decks
                .iter()
                .map(|deck| (
                    deck.seek_request_epoch,
                    deck.seek_complete_epoch,
                    deck.seek_terminal,
                ))
                .collect::<Vec<_>>(),
        ));
        return false;
    }

    let rendered_secs = f64::from(seek_blocks) * BLOCK_FRAMES as f64 / f64::from(case.host_rate);
    let mut positions_valid = true;
    for (deck_index, deck) in decks.iter().enumerate() {
        let Some(served) = deck.player.position_seconds() else {
            positions_valid = false;
            failures.push(format!(
                "{} {label} deck {deck_index} ({}): seek completed without a playback position",
                case.label, deck.observation.label,
            ));
            continue;
        };
        let advance = served - deck.capture_target_secs;
        if advance < -SEEK_POSITION_TOLERANCE_SECS
            || advance > rendered_secs + SEEK_POSITION_TOLERANCE_SECS
        {
            positions_valid = false;
            failures.push(format!(
                "{} {label} deck {deck_index} ({}): seek position {served:.9}s exceeded the {rendered_secs:.9}s rendered budget from {:.9}s",
                case.label, deck.observation.label, deck.capture_target_secs,
            ));
        }
    }
    if !positions_valid {
        return false;
    }

    settle_controls(
        case,
        session,
        decks,
        "post-seek pause",
        EventPolicy::MutedSeekSetup,
        failures,
    )
    .await;
    let active_muted_underruns = decks
        .iter()
        .enumerate()
        .filter_map(|(deck_index, deck)| {
            deck.muted_seek_underrun_epoch
                .map(|seek_epoch| (deck_index, seek_epoch))
        })
        .collect::<Vec<_>>();
    if !active_muted_underruns.is_empty() {
        failures.push(format!(
            "{} {label}: muted seek underruns remained active before gain restore: {active_muted_underruns:?}",
            case.label,
        ));
        return false;
    }
    if let Err(error) = apply_mix(
        decks
            .iter()
            .zip(levels.iter().copied())
            .map(|(deck, level)| (deck.player.as_ref(), level)),
    ) {
        failures.push(format!(
            "{} {label}: apply capture levels failed: {error}",
            case.label,
        ));
        return false;
    }
    for deck in &*decks {
        deck.player.play();
    }
    settle_controls(
        case,
        session,
        decks,
        "gain settle",
        EventPolicy::AudiblePlayback,
        failures,
    )
    .await;
    true
}

async fn settle_controls(
    case: &Case,
    session: &OfflineSession,
    decks: &mut [Deck],
    phase: &str,
    policy: EventPolicy,
    failures: &mut Vec<String>,
) {
    for block_index in 0..CONTROL_SETTLE_BLOCKS {
        let block = render_paced(session, decks, case.host_rate).await;
        if block.len() != BLOCK_FRAMES * usize::from(CHANNELS) {
            failures.push(format!(
                "{} {phase} block {block_index}: produced {} samples",
                case.label,
                block.len(),
            ));
        }
        runtime::drain_all_events(decks, phase, policy, failures);
    }
}

fn disable_mix_tap(case: &str, label: &str, session: &OfflineSession, failures: &mut Vec<String>) {
    match session.exec(Cmd::DisableMixTap) {
        Ok(Reply::Ok) => {}
        Ok(Reply::Err(error)) => {
            failures.push(format!("{case} {label}: disable mix tap returned {error}",))
        }
        Ok(_) => failures.push(format!(
            "{case} {label}: disable mix tap returned an unexpected reply",
        )),
        Err(error) => failures.push(format!(
            "{case} {label}: disable mix tap dispatch failed: {error}",
        )),
    }
}

fn assess_capture(
    case: &str,
    label: &str,
    capture: &[f32],
    tapped: &[f32],
    tap_drops: u64,
    requested_frames: usize,
    zero_blocks: &[usize],
    failures: &mut Vec<String>,
) -> bool {
    let expected_samples = requested_frames * usize::from(CHANNELS);
    if capture.len() != expected_samples {
        failures.push(format!(
            "{case} {label}: PCM shape was {} samples, expected {expected_samples}",
            capture.len(),
        ));
    }
    if !zero_blocks.is_empty() {
        failures.push(format!(
            "{case} {label}: capture contained exact-zero callback blocks at {zero_blocks:?}",
        ));
    }
    if tap_drops != 0 {
        failures.push(format!(
            "{case} {label}: mix tap dropped {tap_drops} samples",
        ));
    }
    let tap_matches = tapped == capture;
    if !tap_matches {
        failures.push(format!(
            "{case} {label}: mix tap did not match graph output bit-exactly (tap={}, output={})",
            tapped.len(),
            capture.len(),
        ));
    }
    tap_matches
}

fn assess_position_advance(
    case: &Case,
    label: &str,
    decks: &mut [Deck],
    positions_before: &[f64],
    requested_frames: usize,
    failures: &mut Vec<String>,
) {
    let duration = f64::from(u32::try_from(requested_frames).expect("capture frames fit u32"))
        / f64::from(case.host_rate);
    for (deck_index, (deck, before)) in decks
        .iter_mut()
        .zip(positions_before.iter().copied())
        .enumerate()
    {
        let after = deck.player.position_seconds().unwrap_or(0.0);
        deck.observation.final_position_secs = after;
        let advance = after - before;
        if (advance - duration).abs() > POSITION_TOLERANCE_SECS {
            failures.push(format!(
                "{} {label} deck {deck_index} ({}): media position advanced {advance:.6}s over {duration:.6}s of output",
                case.label, deck.observation.label,
            ));
        }
    }
}

fn load_decks(case: &Case, decks: &[Deck], failures: &mut Vec<String>) {
    for (deck_index, deck) in decks.iter().enumerate() {
        runtime::record_control_state(case, deck_index, deck, "before playback", failures);
        deck.player
            .select_item_with_crossfade(
                0,
                SelectTransition {
                    autoplay: false,
                    crossfade_seconds: 0.0,
                },
            )
            .unwrap_or_else(|error| {
                panic!("{} deck {deck_index}: select resource: {error}", case.label)
            });
    }
}

async fn prepare_deck(
    case: &Case,
    deck_index: usize,
    media: Media,
    hls: &Url,
    media_dir: &TestTempDir,
    session: &Arc<OfflineSession>,
) -> Deck {
    let controls = StretchControls::new(1.0);
    controls.set_backend(StretchKind::Signalsmith);
    controls.set_keylock(true);
    let bus = EventBus::new(16_384);
    let dispatcher: Arc<dyn SessionDispatcher<TestPools>> = session.clone();
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .bus(bus)
            .sample_rate(
                NonZeroU32::new(case.host_rate).expect("host sample rate must be non-zero"),
            )
            .crossfade_duration(0.0)
            .warp(WarpConfig::builder().stretch(Arc::clone(&controls)).build())
            .session(dispatcher)
            .build(),
    ));
    let events = player.subscribe();
    let src = match media {
        Media::Mp3(asset) => media_path(media_dir, asset)
            .to_str()
            .expect("temporary media path is UTF-8")
            .to_owned(),
        Media::Hls => hls.to_string(),
    };
    let playback_config =
        ResourceConfig::for_src(ResourceSrc::parse(&src).unwrap_or_else(|error| {
            panic!("{} deck {deck_index}: parse {src}: {error}", case.label)
        }))
        .store(memory_asset_store())
        .initial_abr_mode(AbrMode::manual(0))
        .discriminator(format!("{}-deck-{deck_index}-playback", case.label))
        .build();
    let resource = open_resource(
        case,
        deck_index,
        "playback",
        player
            .prepare_config(playback_config)
            .unwrap_or_else(|error| {
                panic!(
                    "{} deck {deck_index}: prepare playback resource: {error}",
                    case.label
                )
            }),
    )
    .await;

    let reference_config =
        ResourceConfig::for_src(ResourceSrc::parse(&src).unwrap_or_else(|error| {
            panic!("{} deck {deck_index}: parse {src}: {error}", case.label)
        }))
        .store(memory_asset_store())
        .worker(player.worker().clone())
        .initial_abr_mode(AbrMode::manual(0))
        .host_sample_rate(NonZeroU32::new(case.host_rate).expect("host rate is non-zero"))
        .events(EventBus::new(16_384))
        .discriminator(format!("{}-deck-{deck_index}-reference", case.label))
        .build();
    let reference = open_resource(case, deck_index, "reference", reference_config).await;
    let reference_events = reference.subscribe();
    player.insert(resource, TrackId::allocate(), None);

    Deck {
        player,
        reference,
        reference_events,
        controls,
        events,
        seek_request_epoch: None,
        seek_complete_epoch: None,
        muted_seek_underrun_epoch: None,
        seek_terminal: false,
        capture_target_secs: CAPTURE_START_SECS + deck_index as f64 * CAPTURE_START_STEP_SECS,
        observation: DeckObservation {
            hls: matches!(media, Media::Hls),
            label: media.label(),
            capture_target_secs: CAPTURE_START_SECS + deck_index as f64 * CAPTURE_START_STEP_SECS,
            ..DeckObservation::default()
        },
    }
}

async fn open_resource(
    case: &Case,
    deck_index: usize,
    role: &str,
    config: ResourceConfig<TestPools>,
) -> Resource {
    let mut resource = time::timeout(PRELOAD_TIMEOUT, Resource::new(config))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{} deck {deck_index}: {role} resource open timed out",
                case.label
            )
        })
        .unwrap_or_else(|error| {
            panic!(
                "{} deck {deck_index}: open {role} resource: {error}",
                case.label
            )
        });
    time::timeout(PRELOAD_TIMEOUT, resource.preload())
        .await
        .unwrap_or_else(|_| panic!("{} deck {deck_index}: {role} preload timed out", case.label))
        .unwrap_or_else(|error| {
            panic!(
                "{} deck {deck_index}: preload {role} resource: {error}",
                case.label
            )
        });
    resource
}

async fn render_paced(session: &OfflineSession, decks: &[Deck], sample_rate: u32) -> Vec<f32> {
    for deck in decks {
        deck.player.process_notifications();
    }
    let block = session.render(BLOCK_FRAMES);
    time::sleep(Duration::from_secs_f64(
        f64::from(u32::try_from(BLOCK_FRAMES).expect("block frames fit u32"))
            / f64::from(sample_rate),
    ))
    .await;
    block
}

fn inspect_block(
    case: &str,
    label: &str,
    block_index: usize,
    block: &[f32],
    zero_blocks: &mut Vec<usize>,
    failures: &mut Vec<String>,
) {
    let expected = BLOCK_FRAMES * usize::from(CHANNELS);
    if block.len() != expected {
        failures.push(format!(
            "{case} {label} callback {block_index}: produced {} samples, expected {expected}",
            block.len(),
        ));
    }
    if block.iter().any(|sample| !sample.is_finite()) {
        failures.push(format!(
            "{case} {label} callback {block_index}: contained non-finite PCM",
        ));
    } else if block.iter().all(|sample| *sample == 0.0) {
        zero_blocks.push(block_index);
    }
}

/// Materializes one generated body as a file the deck can open by path.
fn media_path(dir: &TestTempDir, asset: SignalAsset) -> PathBuf {
    let bytes = by_name(asset.name())
        .unwrap_or_else(|| panic!("`{}` is generated", asset.name()))
        .bytes();
    dir.write(&format!("{}.{}", asset.name(), asset.ext()), bytes)
}
