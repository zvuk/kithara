use std::{num::NonZeroU32, path::PathBuf};

use kithara::{
    assets::{AssetStore, StorageBackend},
    bufpool::PoolRegion,
    events::EventBus,
    hls::AbrMode,
    host::{HostConfig, HostSettings, Tap},
    platform::time::{self, Duration},
    play::{CrossfadeSettings, PlayWorker, PlayWorkerConfig, ResourcePrep, TrackSettings},
    queue::{Queue, QueueConfig, QueueControl, QueueError, QueueSettings, TrackSource, Transition},
    signal::TransportRevision,
    warp::{StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, fixture_protocol::PackagedSignal, memory_asset_store,
    offline::OfflineHostHarness, usdt_trace,
};
#[cfg(not(target_os = "android"))]
use kithara_integration_tests::{audio_artifact::write_audio_artifact, cochlea::CochleaReport};
use kithara_test_fixtures::{SignalAsset, assets::by_name};
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
};
use num_traits::AsPrimitive;
#[cfg(not(target_os = "android"))]
use serde::Serialize;
use url::Url;

#[cfg(not(target_os = "android"))]
use super::oracle::{AudioLevelReport, MatchedMixReport, SampleContinuityReport};
use super::{
    oracle,
    oracle::AudioRole,
    reference::capture_references,
    runtime,
    runtime::{Deck, DeckObservation, EventPolicy},
};

pub(super) const CHANNELS: u16 = 2;
pub(super) const SOURCE_RATE: u32 = 44_100;
pub(super) const BLOCK_FRAMES: usize = 512;
const CAPTURE_SECS: u32 = 2;
const CAPTURE_START_SECS: f64 = 10.0;
const CAPTURE_START_STEP_SECS: f64 = 4.0;
const MAX_SEEK_SECS: u32 = 8;
const CONTROL_SETTLE_BLOCKS: usize = 4;
pub(super) const MIX_HEADROOM: f32 = 0.5;
pub(super) const MIN_FIXED_STEM_RMS_DBFS: f64 = -50.0;
pub(super) const MIN_DECK_CONTRIBUTION_RATIO: f64 = 0.02;
pub(super) const MAX_DECK_GAIN_DELTA: f64 = 0.02;
pub(super) const MAX_MATCHED_RMS_DELTA_DB: f64 = 0.1;
const POSITION_TOLERANCE_SECS: f64 = 0.15;
const SEEK_POSITION_TOLERANCE_SECS: f64 = 513.0 / 44_100.0;
pub(super) const EXACT_ZERO_RUN_LIMIT_FRAMES: usize = 8;
pub(super) const MIN_BOUNDARY_JUMP: f32 = 0.05;
pub(super) const BOUNDARY_OUTLIER_RATIO: f32 = 6.0;
pub(super) const PRELOAD_TIMEOUT: Duration = Duration::from_secs(30);
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

pub(super) struct Case {
    pub(super) label: &'static str,
    pub(super) host_rate: u32,
    /// Channels the case's fixtures decode to. Decoders emit the source's
    /// channel count; the engine mixes to `CHANNELS` planes regardless.
    pub(super) source_channels: u16,
    media: &'static [Media],
}

const MP3_ONE: &[Media] = &[Media::Mp3(SignalAsset::MP3_TRACK_SINE440_187S)];
/// A mono source. Channel count is the only parameter that differs from
/// `MP3_SINE440_60S`: the same tone at the same rate, on one channel.
const MP3_MONO_ONE: &[Media] = &[Media::Mp3(SignalAsset::MP3_MONO_SINE440_60S)];
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
        source_channels: CHANNELS,
        media: MP3_ONE,
    },
    Case {
        label: "no-sync-mp3-distinct-two-48000",
        host_rate: 48_000,
        source_channels: CHANNELS,
        media: MP3_TWO,
    },
    Case {
        label: "no-sync-mp3-alternating-four-44100",
        host_rate: 44_100,
        source_channels: CHANNELS,
        media: MP3_FOUR,
    },
    Case {
        label: "no-sync-mp3-mono-one-48000",
        host_rate: 48_000,
        source_channels: 1,
        media: MP3_MONO_ONE,
    },
    Case {
        label: "no-sync-hls-one-48000",
        host_rate: 48_000,
        source_channels: CHANNELS,
        media: HLS_ONE,
    },
    Case {
        label: "no-sync-hls-mp3-distinct-two-44100",
        host_rate: 44_100,
        source_channels: CHANNELS,
        media: HLS_MP3_TWO,
    },
    Case {
        label: "no-sync-hls-mp3-alternating-four-48000",
        host_rate: 48_000,
        source_channels: CHANNELS,
        media: HLS_MP3_FOUR,
    },
];

#[cfg(not(target_os = "android"))]
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

pub(super) struct CapturedAudio {
    label: String,
    pcm: Vec<f32>,
    pub(super) requested_frames: usize,
    tap_drops: u64,
    tap_matches_output: bool,
    pub(super) start_positions_secs: Vec<f64>,
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
async fn no_sync_real_media_matrix_is_continuous_and_unsynchronized(
    #[future(awt)] real_media_sources: (TestServerHelper, Url, TestTempDir),
) {
    run_real_media_matrix(false, real_media_sources).await;
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(360)))]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_real_media_artifacts(
    #[future(awt)] real_media_sources: (TestServerHelper, Url, TestTempDir),
) {
    run_real_media_matrix(true, real_media_sources).await;
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
    let most_decks: f64 = CASES
        .iter()
        .map(|case| case.media.len())
        .max()
        .expect("the matrix has cases")
        .as_();
    let deepest_capture = (most_decks - 1.0).mul_add(CAPTURE_START_STEP_SECS, CAPTURE_START_SECS)
        + f64::from(CAPTURE_SECS);
    let ladder_segments: f64 = HLS_LADDER_SEGMENTS.as_();
    let ladder_secs = ladder_segments * HLS_LADDER_SEGMENT_SECS;
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

#[kithara::fixture]
async fn real_media_sources() -> (TestServerHelper, Url, TestTempDir) {
    let server = TestServerHelper::new().await;
    let hls = hls_ladder_url(&server).await;
    let dir = TestTempDir::new();
    for case in CASES {
        for media in case.media {
            if let Media::Mp3(asset) = media {
                let _ = media_path(&dir, *asset);
            }
        }
    }
    (server, hls, dir)
}

async fn run_real_media_matrix(
    record_artifacts: bool,
    sources: (TestServerHelper, Url, TestTempDir),
) {
    let (_server, hls, media_dir) = sources;
    let mut failures = Vec::new();
    for case in CASES {
        failures.extend(
            run_case(case, &hls, record_artifacts, &media_dir)
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

async fn run_case(
    case: &Case,
    hls: &Url,
    record_artifacts: bool,
    media_dir: &TestTempDir,
) -> Vec<String> {
    let pool_region = pools();
    let sample_rate = NonZeroU32::new(case.host_rate).expect("host sample rate must be non-zero");
    let max_block_frames =
        NonZeroU32::new(u32::try_from(BLOCK_FRAMES).expect("block frames fit u32"))
            .expect("block frames must be non-zero");
    let _trace = usdt_trace::scope();
    let host = OfflineHostHarness::new(
        HostConfig::offline(pool_region.clone())
            .settings(HostSettings::builder().sample_rate(sample_rate).build())
            .max_block_frames(max_block_frames)
            .build(),
    )
    .await
    .unwrap_or_else(|error| panic!("{}: create product offline Host: {error}", case.label));
    let mut failures = Vec::new();

    let mut decks = Vec::with_capacity(case.media.len());
    let mut items = Vec::with_capacity(case.media.len());
    for (deck_index, media) in case.media.iter().copied().enumerate() {
        let (deck, item) =
            prepare_deck(case, deck_index, media, hls, media_dir, &pool_region, &host).await;
        decks.push(deck);
        items.push(item);
    }

    load_decks(case, &host, &decks, items).await;
    runtime::record_transport_state(&host, None, "before first render", &mut failures).await;
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
        &host,
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
    }
    runtime::record_transport_state(
        &host,
        Some(TransportRevision::first()),
        "after capture",
        &mut failures,
    )
    .await;
    let oracles = oracle::assess_audio(case.label, case.host_rate, &final_mix.pcm, &mut failures);
    tracing::debug!(
        cochlea = ?oracles.cochlea,
        tap_drops = final_mix.tap_drops,
        tap_matches_output = final_mix.tap_matches_output,
        discontinuities = ?oracles.sample_continuity.as_ref().map(|report| &report.discontinuity_boundaries),
        "real-media continuity assessment"
    );

    let signals = listened_signals(&direct_references, &matched_mix, &final_mix);
    let audio_levels = signals
        .iter()
        .map(|(label, role, pcm)| oracle::measure_audio_level(label, *role, case.host_rate, pcm))
        .collect::<Vec<_>>();
    oracle::assess_listening_levels(case.label, &audio_levels, &mut failures);

    #[cfg(target_os = "android")]
    assert!(
        !record_artifacts,
        "listening artifact export belongs to the host suite"
    );
    #[cfg(not(target_os = "android"))]
    if record_artifacts {
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
        let audio_slices = signals
            .iter()
            .map(|(label, _, pcm)| (label.as_str(), *pcm))
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
    host.close().await;
    failures
}

/// Every signal a case listens to, labelled with its role: each deck's direct
/// reference, each deck's contribution to the matched mix, the reference mix,
/// and the final mix.
fn listened_signals<'a>(
    direct_references: &'a [Vec<f32>],
    matched_mix: &'a oracle::MatchedMixAudio,
    final_mix: &'a CapturedAudio,
) -> Vec<(String, AudioRole, &'a [f32])> {
    let references = direct_references
        .iter()
        .enumerate()
        .map(|(deck_index, reference)| {
            (
                format!("direct-reference-{deck_index}"),
                AudioRole::DirectReference,
                reference.as_slice(),
            )
        });
    let contributions =
        matched_mix
            .contributions
            .iter()
            .enumerate()
            .map(|(deck_index, contribution)| {
                (
                    format!("contribution-{deck_index}"),
                    AudioRole::Contribution,
                    contribution.as_slice(),
                )
            });
    references
        .chain(contributions)
        .chain([
            (
                "reference-mix".to_owned(),
                AudioRole::ReferenceMix,
                matched_mix.reference.as_slice(),
            ),
            (
                final_mix.label.clone(),
                AudioRole::FinalMix,
                final_mix.pcm.as_slice(),
            ),
        ])
        .collect()
}

async fn capture_pass(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    label: &str,
    levels: &[f32],
    failures: &mut Vec<String>,
) -> CapturedAudio {
    let ready = reset_for_capture(case, host, decks, label, levels, failures).await;
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

    let mut tap = host
        .attach_tap(
            Tap::Master,
            requested_frames * usize::from(CHANNELS) + BLOCK_FRAMES,
        )
        .await
        .unwrap_or_else(|error| panic!("{} {label}: enable mix tap: {error}", case.label));
    let positions_before = decks
        .iter()
        .map(|deck| deck.player.control().position_seconds().unwrap_or(0.0))
        .collect::<Vec<_>>();
    let mut pcm = Vec::with_capacity(requested_frames * usize::from(CHANNELS));
    let mut zero_blocks = Vec::new();
    for block_index in 0..capture_blocks {
        let block = render_paced(host, case.host_rate).await;
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
    command_decks(host, &*decks, |queue| {
        queue.tick().expect("tick a captured deck");
    })
    .await;
    runtime::drain_all_events(
        decks,
        &format!("{label} final drain"),
        EventPolicy::AudiblePlayback,
        failures,
    );
    if !zero_blocks.is_empty() {
        failures.push(format!(
            "{} {label}: capture contained exact-zero callback blocks at {zero_blocks:?}",
            case.label,
        ));
    }

    let tapped = tap.drain();
    let tap_drops = tap.drops();
    let tap_matches_output = assess_capture(
        case.label,
        label,
        &pcm,
        &tapped,
        tap_drops,
        requested_frames,
        failures,
    );
    detach_tap(case.label, label, host, failures).await;
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
    host: &OfflineHostHarness<TestPools>,
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

    if !pause_muted(case, host, decks, label, failures).await
        || !request_capture_seeks(case, host, decks, label, failures).await
    {
        return false;
    }
    let Some(seek_blocks) = render_until_seeked(case, host, decks, label, failures).await else {
        return false;
    };
    seek_positions_within_budget(case, decks, label, seek_blocks, failures)
        && restore_capture_levels(case, host, decks, label, levels, failures).await
}

/// Pause every deck and mute it, each change settled by a few rendered blocks.
async fn pause_muted(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    label: &str,
    failures: &mut Vec<String>,
) -> bool {
    command_decks(host, &*decks, QueueControl::pause).await;
    settle_controls(
        case,
        host,
        decks,
        "pause",
        EventPolicy::AudiblePlayback,
        failures,
    )
    .await;
    if let Err(error) = set_levels(host, decks, vec![0.0; decks.len()]).await {
        failures.push(format!(
            "{} {label}: mute before seek failed: {error}",
            case.label,
        ));
        return false;
    }
    settle_controls(
        case,
        host,
        decks,
        "mute",
        EventPolicy::AudiblePlayback,
        failures,
    )
    .await;
    true
}

/// Seek every deck to its capture start; every request lands before EOF and
/// reports its epoch.
async fn request_capture_seeks(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    label: &str,
    failures: &mut Vec<String>,
) -> bool {
    for deck in &mut *decks {
        deck.seek_request_segment = None;
        deck.seek_complete_segment = None;
        deck.muted_seek_underrun_segment = None;
        deck.seek_terminal = false;
    }
    let targets: Vec<_> = decks
        .iter()
        .map(|deck| (deck.player.control().clone(), deck.capture_target_secs))
        .collect();
    let seeks = host
        .run(move || {
            targets
                .iter()
                .map(|(player, seconds)| player.seek(*seconds))
                .collect::<Vec<_>>()
        })
        .await;
    let mut requested = true;
    for (deck_index, (deck, seek)) in decks.iter_mut().zip(seeks).enumerate() {
        match seek {
            Ok(()) => {
                deck.seek_request_segment = deck
                    .snapshot
                    .lock()
                    .slots
                    .iter()
                    .find_map(|slot| slot.mark.map(|mark| mark.lane.segment.next()));
                if let Some(duration) = deck
                    .player
                    .control()
                    .duration_seconds()
                    .filter(|duration| deck.capture_target_secs >= *duration)
                {
                    requested = false;
                    failures.push(format!(
                        "{} {label} deck {deck_index} ({}): capture start {:.3}s is past EOF at {duration:.3}s",
                        case.label, deck.observation.label, deck.capture_target_secs,
                    ));
                }
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
    if decks.iter().any(|deck| deck.seek_request_segment.is_none()) {
        failures.push(format!(
            "{} {label}: seek request epochs were not observed for every deck: {:?}",
            case.label,
            decks
                .iter()
                .map(|deck| deck.seek_request_segment)
                .collect::<Vec<_>>(),
        ));
        return false;
    }
    true
}

/// Play the muted decks until every one commits its seek output, pausing each
/// as it lands; returns how many blocks that took.
async fn render_until_seeked(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    label: &str,
    failures: &mut Vec<String>,
) -> Option<u32> {
    command_decks(host, &*decks, QueueControl::play).await;
    let mut completed = false;
    let mut seek_blocks = 0_u32;
    for _ in 0..oracle::blocks_for_secs(case.host_rate, MAX_SEEK_SECS) {
        let block = render_paced(host, case.host_rate).await;
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
        let landed = decks.iter().filter(|deck| {
            deck.seek_complete_segment == deck.seek_request_segment
                && deck.muted_seek_underrun_segment.is_none()
                && deck.player.control().is_playing()
        });
        command_decks(host, landed, QueueControl::pause).await;
        if decks.iter().any(|deck| deck.seek_terminal) {
            break;
        }
        if decks.iter().all(|deck| {
            deck.seek_complete_segment == deck.seek_request_segment
                && deck.muted_seek_underrun_segment.is_none()
        }) {
            completed = true;
            break;
        }
    }
    command_decks(host, &*decks, QueueControl::pause).await;
    if !completed {
        failures.push(format!(
            "{} {label}: not every deck committed seek output within {MAX_SEEK_SECS}s; completions={:?}",
            case.label,
            decks
                .iter()
                .map(|deck| (
                    deck.seek_request_segment,
                    deck.seek_complete_segment,
                    deck.seek_terminal,
                ))
                .collect::<Vec<_>>(),
        ));
        return None;
    }
    Some(seek_blocks)
}

/// Every deck plays from its capture start, advanced by no more than the
/// `seek_blocks` rendered while it landed.
fn seek_positions_within_budget(
    case: &Case,
    decks: &[Deck],
    label: &str,
    seek_blocks: u32,
    failures: &mut Vec<String>,
) -> bool {
    let block_frames: f64 = BLOCK_FRAMES.as_();
    let rendered_secs = f64::from(seek_blocks) * block_frames / f64::from(case.host_rate);
    let mut positions_valid = true;
    for (deck_index, deck) in decks.iter().enumerate() {
        let Some(served) = deck.player.control().position_seconds() else {
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
    positions_valid
}

/// Settle the paused decks, then restore the capture `levels` and play.
async fn restore_capture_levels(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    label: &str,
    levels: &[f32],
    failures: &mut Vec<String>,
) -> bool {
    settle_controls(
        case,
        host,
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
            deck.muted_seek_underrun_segment
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
    if let Err(error) = set_levels(host, decks, levels.to_vec()).await {
        failures.push(format!(
            "{} {label}: apply capture levels failed: {error}",
            case.label,
        ));
        return false;
    }
    command_decks(host, &*decks, QueueControl::play).await;
    settle_controls(
        case,
        host,
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
    host: &OfflineHostHarness<TestPools>,
    decks: &mut [Deck],
    phase: &str,
    policy: EventPolicy,
    failures: &mut Vec<String>,
) {
    for block_index in 0..CONTROL_SETTLE_BLOCKS {
        let block = render_paced(host, case.host_rate).await;
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

async fn detach_tap(
    case: &str,
    label: &str,
    host: &OfflineHostHarness<TestPools>,
    failures: &mut Vec<String>,
) {
    if let Err(error) = host.detach_tap(Tap::Master).await {
        failures.push(format!(
            "{case} {label}: disable mix tap dispatch failed: {error}",
        ));
    }
}

fn assess_capture(
    case: &str,
    label: &str,
    capture: &[f32],
    tapped: &[f32],
    tap_drops: u64,
    requested_frames: usize,
    failures: &mut Vec<String>,
) -> bool {
    let expected_samples = requested_frames * usize::from(CHANNELS);
    if capture.len() != expected_samples {
        failures.push(format!(
            "{case} {label}: PCM shape was {} samples, expected {expected_samples}",
            capture.len(),
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
        let after = deck.player.control().position_seconds().unwrap_or(0.0);
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

/// Hand each deck the item it plays, paused.
async fn load_decks(
    case: &Case,
    host: &OfflineHostHarness<TestPools>,
    decks: &[Deck],
    items: Vec<TrackSource<TestPools>>,
) {
    for (deck_index, (deck, source)) in decks.iter().zip(items).enumerate() {
        let player = deck.player.control().clone();
        let mut events = player.subscribe();
        let id = host
            .run(move || {
                let id = player.append(source)?;
                player.select(id, Transition::None)?;
                player.pause();
                Ok::<_, QueueError>(id)
            })
            .await
            .unwrap_or_else(|error| {
                panic!("{} deck {deck_index}: select source: {error}", case.label)
            });
        kithara_integration_tests::waits::wait_for_loader_done_event(
            &mut events,
            deck.player.control(),
            id,
            PRELOAD_TIMEOUT,
        )
        .await
        .expect("real media loads through its deck");
    }
}

/// Hand each deck its mix level from the host owner thread, stopping at the first refusal.
async fn set_levels(
    host: &OfflineHostHarness<TestPools>,
    decks: &[Deck],
    levels: Vec<f32>,
) -> Result<(), QueueError> {
    let players: Vec<_> = decks
        .iter()
        .map(|deck| deck.player.control().clone())
        .collect();
    host.run(move || {
        players
            .iter()
            .zip(levels)
            .try_for_each(|(player, level)| player.set_level(level))
    })
    .await
}

/// Run `command` on each of `decks` from the host owner thread, the way
/// product callers issue it.
async fn command_decks<'a>(
    host: &OfflineHostHarness<TestPools>,
    decks: impl IntoIterator<Item = &'a Deck>,
    command: fn(&QueueControl<TestPools>),
) {
    let players: Vec<_> = decks
        .into_iter()
        .map(|deck| deck.player.control().clone())
        .collect();
    host.run(move || players.iter().for_each(command)).await;
}

async fn prepare_deck(
    case: &Case,
    deck_index: usize,
    media: Media,
    hls: &Url,
    media_dir: &TestTempDir,
    pool_region: &PoolRegion<TestPools>,
    host: &OfflineHostHarness<TestPools>,
) -> (Deck, TrackSource<TestPools>) {
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pool_region.clone()).build());
    let prep = ResourcePrep::builder()
        .worker(worker.clone())
        .warp(
            WarpConfig::builder()
                .backend(StretchKind::Signalsmith)
                .keylock(true)
                .build(),
        )
        .block_on_underrun(true)
        .build();
    let store = AssetStore::builder(pool_region.clone())
        .backend(StorageBackend::Disk {
            root: media_dir
                .path()
                .join(format!("{}-deck-{deck_index}-assets", case.label)),
        })
        .build();
    let player = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .store(store)
            .track(
                TrackSettings::builder()
                    .backend(StretchKind::Signalsmith)
                    .keylock(true)
                    .build(),
            )
            .settings(
                QueueSettings::builder()
                    .crossfade(CrossfadeSettings {
                        duration: 0.0,
                        ..Default::default()
                    })
                    .build(),
            )
            .build(),
    );
    let src = match media {
        Media::Mp3(asset) => media_dir
            .path()
            .join(format!("{}.{}", asset.name(), asset.ext()))
            .to_str()
            .expect("temporary media path is UTF-8")
            .to_owned(),
        Media::Hls => hls.to_string(),
    };
    let bus = EventBus::new(16_384);
    let events = bus.subscribe();
    let source = TrackSource::Config(Box::new(
        kithara::play::ResourceConfig::for_src(
            kithara::play::ResourceSrc::parse(&src).expect("real media source"),
        )
        .store(memory_asset_store())
        .events(bus)
        .initial_abr_mode(AbrMode::manual(0))
        .discriminator(format!("{}-deck-{deck_index}-playback", case.label))
        .build(),
    ));
    let reference = super::reference::open_reference(
        &worker,
        &src,
        matches!(media, Media::Hls),
        case.host_rate,
    )
    .await;
    let reference_events = reference.subscribe();
    let (player, snapshot) = host
        .insert_observed(player)
        .await
        .expect("insert real media deck");
    let deck_offset: f64 = deck_index.as_();
    let capture_target_secs = deck_offset.mul_add(CAPTURE_START_STEP_SECS, CAPTURE_START_SECS);
    let deck = Deck {
        player,
        snapshot,
        reference,
        reference_events,
        events,
        seek_request_segment: None,
        seek_complete_segment: None,
        muted_seek_underrun_segment: None,
        seek_terminal: false,
        capture_target_secs,
        observation: DeckObservation {
            hls: matches!(media, Media::Hls),
            label: media.label(),
            capture_target_secs,
            ..DeckObservation::default()
        },
    };
    (deck, source)
}

/// Consume one block from every deck and let the clock advance by exactly its
/// duration.
///
/// The guard puts that advance on the same clock as the decks' producers, which
/// are registered pacers: the block period elapses only once they have parked.
/// Without it the sleep is a real `tokio` timer — the test macro rewrites time
/// calls in the test body, not in the helpers it calls — and the consumer would
/// drain the rings at host speed against producers advancing at virtual speed,
/// making every captured window a property of the machine.
#[kithara::flash(true)]
async fn render_paced(host: &OfflineHostHarness<TestPools>, sample_rate: u32) -> Vec<f32> {
    let block = host.render(BLOCK_FRAMES).await;
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
